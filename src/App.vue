<script setup lang="ts">
import { reactive, ref, computed, watch, onMounted, onUnmounted } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import CodeEditor from './CodeEditor.vue';

// ---------- 类型 ----------
interface ConnConfig {
  host: string;
  port: number;
  username: string;
  password: string;
  clientId: string;
}
type Format = 'json' | 'protobuf' | 'plaintext';
interface ProtoField {
  fieldNumber: number | null;
  type: string;
  value: string;
}
interface PublishConfig {
  topic: string;
  payload: string;
  qos: number;
  retain: boolean;
  format: Format;
  mode: 'manual' | 'count' | 'duration';
  count: number;
  intervalMs: number;
  durationSec: number;
  durationUnit: 'sec' | 'hour';
  autoSeq: boolean;
  protoFields: ProtoField[];
  protoMode: 'fields' | 'hex';
}
interface Profile {
  id: string;
  name: string;
  conn: ConnConfig;
  subscriptions: string[];
  publish: PublishConfig;
  timeoutMs: number;
}
interface Message {
  id: number;
  time: string;
  topic: string;
  direction: 'send' | 'recv';
  qos: number;
  content: string;
  seq: number | null;
  status: 'waiting' | 'success' | 'timeout' | 'none';
  sendAt: number;
}

// ---------- 消息格式 ----------
const FORMAT_TEMPLATES: Record<Format, string> = {
  json: '{\n  "seq": {{seq}},\n  "cmd": "down",\n  "ts": {{timestamp}}\n}',
  protobuf: '0a0548656c6c6f',
  plaintext: 'hello {{seq}}',
};
const FORMAT_HINTS: Record<Format, string> = {
  json: '支持变量 {{seq}} {{timestamp}} {{datetime}} {{clientId}}；新一批从 1 递增，暂停续发接着计数',
  protobuf: '字段表格：填字段号/类型/值，发送时自动编码为二进制；Hex 原始：手填 hex 字节。string 值支持变量替换',
  plaintext: '支持变量 {{seq}} {{timestamp}} {{datetime}} {{clientId}}；新一批从 1 递增，暂停续发接着计数',
};
// protobuf 字段表格支持的类型（第一版仅标量，不含嵌套 message / repeated）
const PROTO_TYPES = [
  'int32', 'int64', 'uint32', 'uint64', 'sint32', 'sint64', 'bool', 'enum',
  'string', 'bytes', 'float', 'double', 'fixed32', 'fixed64', 'sfixed32', 'sfixed64',
];
// 记住各格式各自的内容，切换时不丢失
const savedPayloads: Record<Format, string> = { ...FORMAT_TEMPLATES };

function defaultConn(): ConnConfig {
  return { host: '127.0.0.1', port: 1883, username: '', password: '', clientId: '' };
}
function defaultPublish(): PublishConfig {
  return {
    topic: '',
    payload: FORMAT_TEMPLATES.json,
    qos: 1,
    retain: false,
    format: 'json',
    mode: 'manual',
    count: 10,
    intervalMs: 1000,
    durationSec: 60,
    durationUnit: 'sec',
    autoSeq: false,
    protoFields: [],
    protoMode: 'fields',
  };
}
function newId() {
  return 'p-' + Date.now().toString(36) + Math.random().toString(36).slice(2, 6);
}

// ---------- 当前运行时状态（从激活方案同步） ----------
const conn = reactive<ConnConfig>(defaultConn());
const subscriptions = ref<string[]>([]);
const publish = reactive<PublishConfig>(defaultPublish());
const timeoutMs = ref(10000);

const connected = ref(false);
const connecting = ref(false);
const reconnecting = ref(false);
const statusText = ref('未连接');
const errorMsg = ref('');
function showError(msg: string) {
  errorMsg.value = msg;
}
function closeError() {
  errorMsg.value = '';
}

const subInput = ref('');
// 订阅主题行内编辑状态
const editingSub = ref<string | null>(null);
const editingSubValue = ref('');
const subQos = ref(0);

const messages = ref<Message[]>([]);
const filter = ref<'all' | 'send' | 'recv'>('all');
const statusFilter = ref<'all' | 'success' | 'timeout' | 'waiting'>('all');
const topicSearch = ref('');
const detailMsg = ref<Message | null>(null);
let seq = 1;
let timer: number | null = null;
let stopping = false;
// 发送任务状态：idle 空闲 / running 发送中 / paused 已暂停（断点保留，可续发）
type SendState = 'idle' | 'running' | 'paused';
const sendState = ref<SendState>('idle');
const pausedLeft = ref(0); // 按数量：剩余包数
const pausedLeftMs = ref(0); // 按时长：剩余毫秒
let msgId = 0;
let unlistenMsg: UnlistenFn | null = null;
let unlistenStatus: UnlistenFn | null = null;

// ---------- 方案（Profile）管理 ----------
const profiles = ref<Profile[]>([]);
const activeProfileId = ref('');
const activeProfile = computed(() => profiles.value.find((p) => p.id === activeProfileId.value));

// 连接设置弹窗
const showConnModal = ref(false);
const connForm = reactive<ConnConfig>(defaultConn());
// 方案弹窗（新建/重命名）
const showProfileModal = ref(false);
const profileModalMode = ref<'new' | 'rename'>('new');
const profileNameInput = ref('');
// 删除确认弹窗
const showDeleteModal = ref(false);

function applyProfile(p: Profile) {
  Object.assign(conn, p.conn);
  subscriptions.value = [...p.subscriptions];
  Object.assign(publish, p.publish);
  // 数组深拷贝，避免多方案共享同一个 protoFields 引用
  publish.protoFields = (p.publish.protoFields || []).map((f) => ({ ...f }));
  timeoutMs.value = p.timeoutMs;
  Object.assign(savedPayloads, FORMAT_TEMPLATES);
  savedPayloads[publish.format] = publish.payload;
}

function saveActiveProfile() {
  const p = profiles.value.find((x) => x.id === activeProfileId.value);
  if (p) {
    p.conn = { ...conn };
    p.subscriptions = [...subscriptions.value];
    p.publish = { ...publish, protoFields: publish.protoFields.map((f) => ({ ...f })) };
    p.timeoutMs = timeoutMs.value;
  }
}

const LS_KEY = 'mqtt-downlink-config';
function persistProfiles() {
  localStorage.setItem(
    LS_KEY,
    JSON.stringify({ profiles: profiles.value, activeProfileId: activeProfileId.value }),
  );
}
function saveConfig() {
  saveActiveProfile();
  persistProfiles();
}

function normalizeProfile(raw: any): Profile {
  const publish = { ...defaultPublish(), ...(raw.publish || {}) };
  // 迁移占位模板（"cmd": "cmd"）→ 递增变量模板
  if (publish.format === 'json' && publish.payload && publish.payload.includes('"cmd": "cmd"')) {
    publish.payload = FORMAT_TEMPLATES.json;
  }
  // protobuf 字段表格：兜底补齐每个字段行的默认值
  publish.protoFields = (publish.protoFields || []).map((f: any) => ({
    fieldNumber: typeof f.fieldNumber === 'number' ? f.fieldNumber : null,
    type: f.type || 'string',
    value: f.value || '',
  }));
  return {
    id: raw.id || newId(),
    name: raw.name || '默认方案',
    conn: { ...defaultConn(), ...(raw.conn || {}) },
    subscriptions: Array.isArray(raw.subscriptions) ? raw.subscriptions : [],
    publish,
    timeoutMs: raw.timeoutMs || 10000,
  };
}

function loadProfiles() {
  const raw = localStorage.getItem(LS_KEY);
  if (!raw) return;
  try {
    const c = JSON.parse(raw);
    if (Array.isArray(c.profiles)) {
      profiles.value = c.profiles.map((p: any) => normalizeProfile(p));
      activeProfileId.value = c.activeProfileId || profiles.value[0]?.id || '';
    } else if (c.conn) {
      // 旧格式迁移：单套配置 → 一个默认方案
      const p = normalizeProfile({
        id: newId(),
        name: '默认方案',
        conn: c.conn,
        subscriptions: c.subscriptions,
        publish: c.publish,
        timeoutMs: c.timeoutMs,
      });
      profiles.value = [p];
      activeProfileId.value = p.id;
    }
  } catch {
    /* 忽略 */
  }
}

function makeProfile(name: string): Profile {
  return {
    id: newId(),
    name,
    conn: { ...conn }, // 继承当前连接配置
    subscriptions: [],
    publish: { ...defaultPublish(), topic: '' },
    timeoutMs: 10000,
  };
}

function switchProfile(id: string) {
  if (id === activeProfileId.value) return;
  if (connected.value) doDisconnect();
  saveActiveProfile();
  saveSendpoint(activeProfileId.value); // 切走前保存当前方案断点
  activeProfileId.value = id;
  const p = profiles.value.find((x) => x.id === id);
  if (p) applyProfile(p);
  messages.value = [];
  seq = 1;
  resetSendState();
  loadSendpoint(id); // 切到目标方案后恢复其断点
  persistProfiles();
}
function onProfileSelectChange(e: Event) {
  switchProfile((e.target as HTMLSelectElement).value);
}

function openNewProfile() {
  profileModalMode.value = 'new';
  profileNameInput.value = '';
  showProfileModal.value = true;
}
function openRename() {
  profileModalMode.value = 'rename';
  profileNameInput.value = activeProfile.value?.name || '';
  showProfileModal.value = true;
}
function confirmProfileModal() {
  const name = profileNameInput.value.trim();
  if (!name) return;
  if (profileModalMode.value === 'new') {
    saveActiveProfile();
    const p = makeProfile(name);
    profiles.value.push(p);
    activeProfileId.value = p.id;
    applyProfile(p);
    messages.value = [];
    seq = 1;
    resetSendState();
  } else {
    const p = profiles.value.find((x) => x.id === activeProfileId.value);
    if (p) p.name = name;
  }
  showProfileModal.value = false;
  persistProfiles();
}
function openDeleteModal() {
  if (profiles.value.length <= 1) return;
  showDeleteModal.value = true;
}
function confirmDelete() {
  const deletedId = activeProfileId.value;
  profiles.value = profiles.value.filter((x) => x.id !== deletedId);
  clearSendpoint(deletedId);
  activeProfileId.value = profiles.value[0].id;
  applyProfile(profiles.value[0]);
  messages.value = [];
  seq = 1;
  resetSendState();
  persistProfiles();
  showDeleteModal.value = false;
}

// ---------- 连接设置弹窗 ----------
function openConnModal() {
  connForm.host = conn.host;
  connForm.port = conn.port;
  connForm.username = conn.username;
  connForm.password = conn.password;
  connForm.clientId = conn.clientId;
  showConnModal.value = true;
}
function closeConnModal() {
  showConnModal.value = false;
}
function saveConnModal() {
  if (!connForm.host) return;
  Object.assign(conn, connForm);
  showConnModal.value = false;
  saveConfig();
}
function genClientIdFor(target: ConnConfig) {
  target.clientId = 'dl-tool-' + Math.random().toString(16).slice(2, 10);
}

// ---------- 消息格式切换 ----------
function switchFormat(f: Format) {
  if (f === publish.format) return;
  savedPayloads[publish.format] = publish.payload;
  publish.format = f;
  publish.payload = savedPayloads[f] ?? FORMAT_TEMPLATES[f];
  saveConfig();
}
const formatHint = computed(() => FORMAT_HINTS[publish.format]);

// ---------- protobuf 字段表格 ----------
function switchProtoMode(m: 'fields' | 'hex') {
  if (publish.protoMode === m) return;
  publish.protoMode = m;
  saveConfig();
}
function addProtoField() {
  publish.protoFields.push({ fieldNumber: null, type: 'string', value: '' });
  saveConfig();
}
function removeProtoField(i: number) {
  publish.protoFields.splice(i, 1);
  saveConfig();
}
function protoValuePlaceholder(type: string) {
  if (type === 'string') return '字符串，支持 {{seq}} 等变量';
  if (type === 'bytes') return 'hex 编码，如 0a0548656c6c6f';
  if (type === 'bool') return 'true / false';
  if (type === 'float' || type === 'double') return '如 3.14';
  return '整数';
}

// ---------- protobuf 实时预览（前端 TS 编码，与 Rust proto.rs 对齐；仅用于预览，实际发送以 Rust 端为准） ----------
function writeVarint(out: number[], v: bigint) {
  while (true) {
    let b = Number(v & 0x7fn);
    v >>= 7n;
    if (v !== 0n) b |= 0x80;
    out.push(b);
    if (v === 0n) break;
  }
}
function parseBigInt(s: string): bigint | null {
  const t = s.trim();
  if (!/^-?\d+$/.test(t)) return null;
  try {
    return BigInt(t);
  } catch {
    return null;
  }
}
function hexToBytes(s: string): number[] {
  const cleaned = s.replace(/\s/g, '').replace(/^0x/i, '');
  if (cleaned.length === 0 || cleaned.length % 2 !== 0) {
    return Array.from(new TextEncoder().encode(s));
  }
  const out: number[] = [];
  for (let i = 0; i < cleaned.length; i += 2) {
    const b = parseInt(cleaned.slice(i, i + 2), 16);
    if (isNaN(b)) return Array.from(new TextEncoder().encode(s));
    out.push(b);
  }
  return out;
}
function encodeProtoPreview(fields: ProtoField[]): number[] {
  const out: number[] = [];
  const enc = new TextEncoder();
  for (const f of fields) {
    const num = f.fieldNumber ?? 0;
    const val = f.value;
    let wireType = 0;
    let payload: number[] = [];
    switch (f.type) {
      case 'int32':
      case 'int64': {
        const v = parseBigInt(val);
        if (v == null) throw new Error(`字段 ${num} 的 int 值「${val}」不是合法整数`);
        wireType = 0;
        writeVarint(payload, BigInt.asUintN(64, v));
        break;
      }
      case 'uint32':
      case 'uint64':
      case 'enum': {
        const v = parseBigInt(val);
        if (v == null || v < 0n) throw new Error(`字段 ${num} 的值「${val}」不是非负整数`);
        wireType = 0;
        writeVarint(payload, v);
        break;
      }
      case 'sint32': {
        const v = parseBigInt(val);
        if (v == null) throw new Error(`字段 ${num} 的 sint32 值「${val}」不是合法整数`);
        wireType = 0;
        writeVarint(payload, BigInt.asUintN(32, (v << 1n) ^ (v >> 31n)));
        break;
      }
      case 'sint64': {
        const v = parseBigInt(val);
        if (v == null) throw new Error(`字段 ${num} 的 sint64 值「${val}」不是合法整数`);
        wireType = 0;
        writeVarint(payload, BigInt.asUintN(64, (v << 1n) ^ (v >> 63n)));
        break;
      }
      case 'bool': {
        wireType = 0;
        writeVarint(payload, ['true', '1', 'yes'].includes(val.trim().toLowerCase()) ? 1n : 0n);
        break;
      }
      case 'string': {
        wireType = 2;
        const bytes = Array.from(enc.encode(val));
        writeVarint(payload, BigInt(bytes.length));
        payload.push(...bytes);
        break;
      }
      case 'bytes': {
        wireType = 2;
        const bytes = hexToBytes(val);
        writeVarint(payload, BigInt(bytes.length));
        payload.push(...bytes);
        break;
      }
      case 'float': {
        const x = Number(val.trim());
        if (isNaN(x)) throw new Error(`字段 ${num} 的 float 值「${val}」不是合法小数`);
        wireType = 5;
        const buf = new ArrayBuffer(4);
        new DataView(buf).setFloat32(0, x, true);
        payload = Array.from(new Uint8Array(buf));
        break;
      }
      case 'fixed32':
      case 'sfixed32': {
        const v = parseBigInt(val);
        if (v == null) throw new Error(`字段 ${num} 的值「${val}」不是合法整数`);
        wireType = 5;
        const buf = new ArrayBuffer(4);
        new DataView(buf).setUint32(0, Number(BigInt.asUintN(32, v)), true);
        payload = Array.from(new Uint8Array(buf));
        break;
      }
      case 'double': {
        const x = Number(val.trim());
        if (isNaN(x)) throw new Error(`字段 ${num} 的 double 值「${val}」不是合法小数`);
        wireType = 1;
        const buf = new ArrayBuffer(8);
        new DataView(buf).setFloat64(0, x, true);
        payload = Array.from(new Uint8Array(buf));
        break;
      }
      case 'fixed64':
      case 'sfixed64': {
        const v = parseBigInt(val);
        if (v == null) throw new Error(`字段 ${num} 的值「${val}」不是合法整数`);
        wireType = 1;
        const u = BigInt.asUintN(64, v);
        const buf = new ArrayBuffer(8);
        const dv = new DataView(buf);
        dv.setUint32(0, Number(u & 0xffffffffn), true);
        dv.setUint32(4, Number((u >> 32n) & 0xffffffffn), true);
        payload = Array.from(new Uint8Array(buf));
        break;
      }
      default:
        throw new Error(`不支持的 protobuf 类型「${f.type}」`);
    }
    writeVarint(out, (BigInt(num) << 3n) | BigInt(wireType));
    out.push(...payload);
  }
  return out;
}
const protoPreview = computed(() => {
  if (publish.format !== 'protobuf' || publish.protoMode !== 'fields') return '';
  const fields = publish.protoFields.filter(
    (f) => f.fieldNumber != null && f.fieldNumber >= 1 && f.value !== '',
  );
  if (fields.length === 0) return '';
  try {
    const bytes = encodeProtoPreview(fields);
    return '0x' + bytes.map((b) => b.toString(16).padStart(2, '0')).join('');
  } catch (e) {
    return '编码错误：' + (e as Error).message;
  }
});

// 暂停态提示（显示剩余量，说明续发不会重置序号）
const pausedTip = computed(() => {
  if (sendState.value !== 'paused') return '';
  if (publish.mode === 'count') {
    return `已暂停：剩余 ${pausedLeft.value} 包未发送，点「继续发送」接着发（序号不重置）`;
  }
  return `已暂停：剩余 ${Math.max(1, Math.ceil(pausedLeftMs.value / 1000))} 秒，点「继续发送」接着发（序号不重置）`;
});

// ---------- 统计 ----------
const stats = computed(() => {
  const sent = messages.value.filter((m) => m.direction === 'send').length;
  const success = messages.value.filter((m) => m.status === 'success').length;
  const timeout = messages.value.filter((m) => m.status === 'timeout').length;
  const waiting = messages.value.filter((m) => m.status === 'waiting').length;
  const lossRate = sent > 0 ? ((timeout / sent) * 100).toFixed(1) : '0.0';
  return { sent, success, timeout, waiting, lossRate };
});

const filteredMessages = computed(() => {
  let list = messages.value;
  if (filter.value !== 'all') {
    list = list.filter((m) =>
      filter.value === 'send' ? m.direction === 'send' : m.direction === 'recv',
    );
  }
  if (statusFilter.value !== 'all') {
    list = list.filter((m) => m.status === statusFilter.value);
  }
  const kw = topicSearch.value.trim();
  if (kw) {
    list = list.filter((m) => m.topic.includes(kw));
  }
  // 新消息排最上（倒序展示，不改变内部数组顺序，保证配对逻辑稳定）
  return [...list].reverse();
});

// ---------- 工具函数 ----------
function pad(n: number, len = 2) {
  return String(n).padStart(len, '0');
}
function timeStr(d = new Date()) {
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}
function fullDateTime(d = new Date()) {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}

// 变量替换 {{seq}} {{timestamp}} {{datetime}} {{clientId}}
function resolveVariables(template: string, s: number) {
  const now = new Date();
  return template
    .replace(/\{\{\s*seq\s*\}\}/g, String(s))
    .replace(/\{\{\s*timestamp\s*\}\}/g, String(now.getTime()))
    .replace(/\{\{\s*datetime\s*\}\}/g, fullDateTime(now))
    .replace(/\{\{\s*clientId\s*\}\}/g, conn.clientId);
}

// 勾选「自动填充序号」时，把 {{seq}} 变量一次性填入消息内容（取消勾选不回退已有内容）
function toggleAutoSeq() {
  if (!publish.autoSeq || publish.format === 'protobuf') return;
  if (publish.payload.includes('{{seq}}')) return;
  if (publish.format === 'json') {
    // JSON 已有 seq 字段则不重复插入
    if (/["']seq["']\s*:/.test(publish.payload)) return;
    publish.payload = publish.payload.replace(/\{/, '{\n  "seq": {{seq}},');
    return;
  }
  // 明文：结尾是数字则替换为变量（hello 1 → hello {{seq}}），否则在末尾追加
  if (/\d+\s*$/.test(publish.payload)) {
    publish.payload = publish.payload.replace(/\d+\s*$/, '{{seq}}');
  } else {
    publish.payload = publish.payload.trimEnd() + ' {{seq}}';
  }
}

// 从回执 payload 中提取 seq（仅从 JSON 字段或明确的 seq= 字样提取，避免误判字面量数字）
function extractSeq(payload: string): number | null {
  try {
    const obj = JSON.parse(payload);
    const v = obj.seq ?? obj.seqNo ?? obj.sequence ?? obj.sn;
    if (typeof v === 'number') return v;
    if (typeof v === 'string' && /^\d+$/.test(v)) return Number(v);
  } catch {
    /* 非 JSON */
  }
  const m = payload.match(/(?:seq|sequence|seqNo|sn)\s*[:=]\s*(\d+)/i);
  if (m) return Number(m[1]);
  return null;
}

// ---------- 消息内容展示 ----------
// 判断是否为二进制 hex 内容（后端把非 UTF-8 二进制回执转成 0x 前缀）
function isHexContent(content: string) {
  return content.startsWith('0x') || content.startsWith('0X');
}
// 详情展示：JSON 美化、hex 原样、其余纯文本
function prettyContent(content: string) {
  if (isHexContent(content)) return content;
  try {
    return JSON.stringify(JSON.parse(content), null, 2);
  } catch {
    return content;
  }
}
function openDetail(m: Message) {
  detailMsg.value = m;
}
function closeDetail() {
  detailMsg.value = null;
}
async function copyMessage(content: string) {
  try {
    await navigator.clipboard.writeText(content);
  } catch {
    const ta = document.createElement('textarea');
    ta.value = content;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand('copy');
    document.body.removeChild(ta);
  }
}

// ---------- MQTT 操作 ----------
async function doConnect() {
  if (!conn.host || !conn.clientId) return;
  connecting.value = true;
  statusText.value = '连接中...';
  try {
    await invoke('connect', { config: { ...conn } });
  } catch (e) {
    connecting.value = false;
    statusText.value = '连接失败';
    showError('连接失败: ' + e);
  }
}
async function doDisconnect() {
  try {
    await invoke('disconnect');
  } catch (e) {
    /* 忽略 */
  }
  connected.value = false;
  connecting.value = false;
  statusText.value = '未连接';
}

async function addSub() {
  const topic = subInput.value.trim();
  if (!topic || subscriptions.value.includes(topic)) return;
  subscriptions.value.push(topic);
  subInput.value = '';
  saveConfig();
  if (connected.value) {
    try {
      await invoke('subscribe', { topic, qos: subQos.value });
    } catch (e) {
      showError('订阅失败: ' + e);
    }
  }
}
async function removeSub(topic: string) {
  subscriptions.value = subscriptions.value.filter((t) => t !== topic);
  saveConfig();
  if (connected.value) {
    try {
      await invoke('unsubscribe', { topic });
    } catch (e) {
      /* 忽略 */
    }
  }
}

// 点击主题进入编辑态
function startEditSub(topic: string) {
  editingSub.value = topic;
  editingSubValue.value = topic;
}
function cancelEditSub() {
  editingSub.value = null;
  editingSubValue.value = '';
}
// 确认编辑：改主题（旧退订 + 新订阅）
async function confirmEditSub() {
  const oldTopic = editingSub.value;
  if (oldTopic == null) return;
  const newTopic = editingSubValue.value.trim();
  if (!newTopic || newTopic === oldTopic) {
    cancelEditSub();
    return;
  }
  if (subscriptions.value.includes(newTopic)) {
    showError('订阅主题已存在: ' + newTopic);
    return;
  }
  editingSub.value = null;
  editingSubValue.value = '';
  subscriptions.value = subscriptions.value.map((t) => (t === oldTopic ? newTopic : t));
  saveConfig();
  if (connected.value) {
    try {
      await invoke('unsubscribe', { topic: oldTopic });
    } catch (e) {
      /* 忽略 */
    }
    try {
      await invoke('subscribe', { topic: newTopic, qos: subQos.value });
    } catch (e) {
      showError('订阅失败: ' + e);
    }
  }
}

async function resubscribeAll() {
  for (const topic of subscriptions.value) {
    try {
      await invoke('subscribe', { topic, qos: subQos.value });
    } catch (e) {
      /* 忽略 */
    }
  }
}

// ---------- 发送 ----------
function sendOne() {
  const isProtoFields = publish.format === 'protobuf' && publish.protoMode === 'fields';
  // 表格模式：过滤未填字段号的行，string 值做变量替换，其余类型原样
  let payload: string;
  let protoFields: ProtoField[] | null = null;
  if (isProtoFields) {
    protoFields = publish.protoFields
      .filter((f) => f.fieldNumber != null && f.fieldNumber >= 1)
      .map((f) => ({
        fieldNumber: f.fieldNumber!,
        type: f.type,
        value: f.type === 'string' ? resolveVariables(f.value, seq) : f.value,
      }));
    if (protoFields.length === 0) {
      showError('protobuf 字段表格为空，请至少填写一个字段（字段号 + 值）');
      return;
    }
    payload = '';
  } else {
    payload =
      publish.format === 'protobuf'
        ? publish.payload
        : resolveVariables(publish.payload, seq);
    // JSON 发送前本地校验，格式错直接拦截，不发出去
    if (publish.format === 'json') {
      try {
        JSON.parse(payload);
      } catch (e) {
        showError('JSON 格式错误，未发送：\n' + (e as Error).message);
        return;
      }
    }
  }
  const rec: Message = {
    id: ++msgId,
    time: timeStr(),
    topic: publish.topic,
    direction: 'send',
    qos: publish.qos,
    content: protoFields
      ? protoFields.map((f) => `${f.fieldNumber}:${f.type}=${f.value}`).join(' ')
      : payload,
    seq,
    status: 'waiting',
    sendAt: Date.now(),
  };
  messages.value.push(rec);
  invoke('publish', {
    config: {
      topic: publish.topic,
      payload,
      qos: publish.qos,
      retain: publish.retain,
      format: publish.format,
      protoFields,
    },
  }).catch((e) => {
    rec.status = 'timeout';
    showError('发送失败: ' + e);
  });
  seq++;
}

// 清理定时器
function clearTimer() {
  if (timer != null) {
    clearInterval(timer);
    timer = null;
  }
}
// 丢弃断点（切方案/切发送方式/自然结束后调用）
function resetSendState() {
  stopping = true;
  clearTimer();
  sendState.value = 'idle';
  pausedLeft.value = 0;
  pausedLeftMs.value = 0;
}

// ---------- 断点持久化（跨会话续发） ----------
type Sendpoint = { mode: string; pausedLeft: number; pausedLeftMs: number; seq: number };
const SEND_KEY = 'mqtt-downlink-sendpoint';
function readSendpoints(): Record<string, Sendpoint> {
  try {
    return JSON.parse(localStorage.getItem(SEND_KEY) || '{}');
  } catch {
    return {};
  }
}
function writeSendpoints(map: Record<string, Sendpoint>) {
  localStorage.setItem(SEND_KEY, JSON.stringify(map));
}
// 暂停时保存当前方案断点；仅 paused 态才写，避免空断点残留
function saveSendpoint(profileId: string) {
  if (sendState.value !== 'paused') return;
  const map = readSendpoints();
  map[profileId] = {
    mode: publish.mode,
    pausedLeft: pausedLeft.value,
    pausedLeftMs: pausedLeftMs.value,
    seq,
  };
  writeSendpoints(map);
}
function clearSendpoint(profileId: string) {
  const map = readSendpoints();
  delete map[profileId];
  writeSendpoints(map);
}
// 恢复方案断点（发送方式需匹配，否则断点语义失效）
function loadSendpoint(profileId: string) {
  const s = readSendpoints()[profileId];
  if (!s || s.mode !== publish.mode) return;
  pausedLeft.value = s.pausedLeft || 0;
  pausedLeftMs.value = s.pausedLeftMs || 0;
  seq = s.seq || 1;
  if (pausedLeft.value > 0 || pausedLeftMs.value > 0) {
    sendState.value = 'paused';
  }
}

function startSend() {
  if (publish.mode === 'manual') {
    sendOne();
    return;
  }
  if (sendState.value === 'running') return;
  stopping = false;

  if (publish.mode === 'count') {
    // 有断点则续发（seq 不重置）；否则是新任务，seq 从 1 开始（方案 A）
    const resuming = sendState.value === 'paused' && pausedLeft.value > 0;
    if (!resuming) {
      seq = 1;
      pausedLeft.value = Math.max(1, publish.count);
    }
    sendState.value = 'running';
    const tick = () => {
      if (stopping || pausedLeft.value <= 0) {
        finishRun();
        return;
      }
      sendOne();
      pausedLeft.value--;
      // 最后一包发完立即收尾，避免按钮多亮一个间隔
      if (pausedLeft.value <= 0) finishRun();
    };
    tick();
    if (!stopping && sendState.value === 'running') {
      timer = window.setInterval(tick, publish.intervalMs);
    }
  } else {
    const unitMs = publish.durationUnit === 'hour' ? 3600_000 : 1000;
    // 有断点则只跑剩余时长且 seq 不重置；否则是新任务，seq 从 1 开始
    const resuming = sendState.value === 'paused' && pausedLeftMs.value > 0;
    let totalMs: number;
    if (resuming) {
      totalMs = pausedLeftMs.value;
    } else {
      seq = 1;
      totalMs = Math.max(1, publish.durationSec * unitMs);
    }
    sendState.value = 'running';
    const start = Date.now();
    const tick = () => {
      const left = totalMs - (Date.now() - start);
      if (stopping || left <= 0) {
        finishRun();
        return;
      }
      sendOne();
      pausedLeftMs.value = left;
    };
    tick();
    if (!stopping && sendState.value === 'running') {
      timer = window.setInterval(tick, publish.intervalMs);
    }
  }
}
// 任务自然结束：清空断点，下次开始 = 新任务
function finishRun() {
  clearSendpoint(activeProfileId.value);
  resetSendState();
}
// 用户点停止 = 暂停，保留断点（下次点「继续发送」接着发，seq 不重置）
function stopSend() {
  stopping = true;
  clearTimer();
  if (sendState.value === 'running') {
    sendState.value = pausedLeft.value > 0 || pausedLeftMs.value > 0 ? 'paused' : 'idle';
    if (sendState.value === 'paused') saveSendpoint(activeProfileId.value);
  }
}
// 放弃断点，重新从 1 开始发一批
function restartSend() {
  clearSendpoint(activeProfileId.value);
  resetSendState();
  startSend();
}

// ---------- 回执配对 + 超时 ----------
// 严格序号核对：回执必须能对上号才算成功，否则该包保持 waiting 等超时标红。
// 1. 回执带显式 seq（JSON 字段 / seq=、sn= 等）→ 按序号精确匹配；
// 2. 回执无 seq → 按内容精确匹配（回执内容与发送内容一致才算成功）；
// 3. 内容不完全一致时，回执结尾数字须与某条待回执包的序号相同（如 "ack hello 2" 可配 hello 2）；
// 4. 都对不上则不配对（不做 FIFO 顺序兜底，避免序号错配的回执也算成功）。
function onIncoming(msg: { topic: string; payload: string; qos: number; timestamp: number }) {
  const matched = extractSeq(msg.payload);
  let rec: Message | undefined;
  if (matched != null) {
    rec = messages.value.find(
      (m) => m.direction === 'send' && m.status === 'waiting' && m.seq === matched,
    );
  }
  if (!rec) {
    rec = messages.value.find(
      (m) => m.direction === 'send' && m.status === 'waiting' && m.content === msg.payload,
    );
  }
  if (!rec) {
    const tail = msg.payload.match(/(\d+)\s*$/);
    if (tail) {
      const n = Number(tail[1]);
      rec = messages.value.find(
        (m) =>
          m.direction === 'send' &&
          m.status === 'waiting' &&
          m.seq === n &&
          m.content.trimEnd().endsWith(String(n)),
      );
    }
  }
  if (rec) rec.status = 'success';
  messages.value.push({
    id: ++msgId,
    time: timeStr(new Date(msg.timestamp)),
    topic: msg.topic,
    direction: 'recv',
    qos: msg.qos,
    content: msg.payload,
    seq: matched,
    status: 'none',
    sendAt: Date.now(),
  });
}

function checkTimeout() {
  const now = Date.now();
  for (const m of messages.value) {
    if (m.direction === 'send' && m.status === 'waiting' && now - m.sendAt > timeoutMs.value) {
      m.status = 'timeout';
    }
  }
}

// 消息内容变化时同步到格式缓存并自动保存配置
watch(() => publish.payload, () => {
  savedPayloads[publish.format] = publish.payload;
  saveConfig();
});

// protobuf 字段表格变化时持久化
watch(() => publish.protoFields, () => saveConfig(), { deep: true });

// 切换发送方式会作废断点（按数量/按时长的续发语义不同，避免序号与剩余量错配）
watch(() => publish.mode, () => resetSendState());

// ---------- 生命周期 ----------
onMounted(async () => {
  loadProfiles();
  if (profiles.value.length === 0) {
    profiles.value.push(makeProfile('默认方案'));
    activeProfileId.value = profiles.value[0].id;
  }
  if (!profiles.value.find((p) => p.id === activeProfileId.value)) {
    activeProfileId.value = profiles.value[0].id;
  }
  applyProfile(profiles.value.find((p) => p.id === activeProfileId.value)!);
  if (!conn.clientId) genClientIdFor(conn);
  savedPayloads[publish.format] = publish.payload;
  saveConfig();
  loadSendpoint(activeProfileId.value);
  unlistenMsg = await listen('mqtt-message', (e) => onIncoming(e.payload as any));
  unlistenStatus = await listen('mqtt-status', (e) => {
    const s = e.payload as { connected: boolean; message: string };
    connecting.value = false;
    if (s.connected) {
      connected.value = true;
      reconnecting.value = false;
      statusText.value = s.message;
      resubscribeAll();
    } else if (s.message.startsWith('重连中')) {
      connected.value = false;
      reconnecting.value = true;
      statusText.value = '重连中...';
    } else if (s.message.startsWith('连接错误')) {
      // 错误详情弹窗展示，角标只显示短语，避免长文本撑乱布局
      connected.value = false;
      reconnecting.value = false;
      statusText.value = '连接错误';
      showError(s.message);
    } else {
      connected.value = false;
      reconnecting.value = false;
      statusText.value = s.message;
    }
  });
  window.setInterval(checkTimeout, 500);
});
onUnmounted(() => {
  saveConfig();
  stopSend();
  unlistenMsg?.();
  unlistenStatus?.();
});
</script>

<template>
  <div class="app">
    <!-- 顶部栏 -->
    <header class="topbar">
      <div class="brand">
        <span class="logo">⤓</span>
        <span class="title">MQTT Test Tool</span>
        <span class="subtitle">MQTT 5.0 · Downlink Test Tool</span>
      </div>

      <div class="profile-bar">
        <select class="profile-select" :value="activeProfileId" @change="onProfileSelectChange">
          <option v-for="p in profiles" :key="p.id" :value="p.id">{{ p.name }}</option>
        </select>
        <button class="btn ghost" @click="openNewProfile" title="新建方案">+ 新建</button>
        <button class="btn ghost" @click="openRename" title="重命名方案">重命名</button>
        <button
          class="btn ghost"
          :class="{ 'soft-off': profiles.length <= 1 }"
          :disabled="profiles.length <= 1"
          @click="openDeleteModal"
          title="删除当前方案"
        >
          删除
        </button>
      </div>

      <div class="top-right">
        <span class="status-pill" :class="connected ? 'ok' : (connecting || reconnecting) ? 'ing' : 'off'">
          {{ statusText }}
        </span>
        <button v-if="!connected" class="btn primary" :disabled="connecting" @click="doConnect">连接</button>
        <button v-else class="btn danger" @click="doDisconnect">断开</button>
      </div>
    </header>

    <div class="body">
      <!-- 左栏：连接概览 + 订阅 -->
      <aside class="col-left">
        <section class="card overview-card" :class="{ 'conn-broken': reconnecting }" @click="openConnModal" title="点击编辑连接设置">
          <div class="ov-head">
            <span class="ov-icon">⧉</span>
            <div class="ov-title">
              <div class="ov-name">连接概览</div>
              <div class="ov-sub">{{ activeProfile?.name || '' }}</div>
            </div>
            <span class="ov-status" :class="connected ? 'ok' : connecting ? 'ing' : 'off'">
              <span class="dot"></span>{{ statusText }}
            </span>
          </div>
          <div class="ov-rows">
            <div class="ov-row"><span>Broker 地址</span><b>{{ conn.host }}:{{ conn.port }}</b></div>
            <div class="ov-row"><span>用户名</span><b>{{ conn.username || '—' }}</b></div>
            <div class="ov-row"><span>密码</span><b>{{ conn.password ? '••••••••' : '—' }}</b></div>
            <div class="ov-row"><span>Client ID</span><b>{{ conn.clientId || '—' }}</b></div>
            <div class="ov-row"><span>协议版本</span><b>MQTT 5.0</b></div>
          </div>
          <div class="ov-edit">点击编辑连接设置</div>
        </section>

        <section class="card">
          <h3>订阅主题 <span class="hint">（接收设备回执）</span></h3>
          <div class="inline">
            <div class="igroup">
              <span class="iaddon">SUB</span>
              <input v-model="subInput" placeholder="xxx/testDownRes" @keyup.enter="addSub" />
            </div>
            <button class="btn ghost add" @click="addSub">添加</button>
          </div>
          <ul class="sub-list">
            <li v-for="t in subscriptions" :key="t">
              <span class="topic-chip" title="点击编辑" @click="startEditSub(t)">{{ t }}</span>
              <button class="x" @click="removeSub(t)" title="删除">×</button>
            </li>
            <li v-if="subscriptions.length === 0" class="empty">暂无订阅</li>
          </ul>
        </section>
      </aside>

      <!-- 中栏：发布 / 发送 -->
      <section class="col-mid">
        <div class="card">
          <h3>发布 / 发送</h3>
          <div class="field">
            <div class="igroup">
              <span class="iaddon">TOPIC</span>
              <input v-model="publish.topic" placeholder="/communicateCbb/123/testDown" @change="saveConfig" />
            </div>
          </div>

          <div class="field">
            <div class="label-row">
              <label>消息内容</label>
              <div class="format-tabs">
                <button class="tab" :class="{ on: publish.format === 'json' }" @click="switchFormat('json')">JSON</button>
                <button class="tab" :class="{ on: publish.format === 'protobuf' }" @click="switchFormat('protobuf')">Protobuf</button>
                <button class="tab" :class="{ on: publish.format === 'plaintext' }" @click="switchFormat('plaintext')">Plaintext</button>
              </div>
            </div>

            <!-- protobuf 子模式：字段表格 / Hex 原始 -->
            <div v-if="publish.format === 'protobuf'" class="proto-mode-row">
              <div class="seg">
                <button
                  class="seg-btn"
                  :class="{ on: publish.protoMode === 'fields' }"
                  @click="switchProtoMode('fields')"
                >
                  字段表格
                </button>
                <button
                  class="seg-btn"
                  :class="{ on: publish.protoMode === 'hex' }"
                  @click="switchProtoMode('hex')"
                >
                  Hex 原始
                </button>
              </div>
            </div>

            <CodeEditor
              v-if="publish.format !== 'protobuf' || publish.protoMode === 'hex'"
              v-model="publish.payload"
              :format="publish.format"
            />

            <!-- protobuf 字段表格：字段号 / 类型 / 值，发送时自动编码为二进制 -->
            <div v-else class="proto-panel">
              <table class="proto-table">
                <thead>
                  <tr>
                    <th class="c-num">字段号</th>
                    <th class="c-type">类型</th>
                    <th class="c-val">值</th>
                    <th class="c-op"></th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="(f, i) in publish.protoFields" :key="i">
                    <td class="c-num">
                      <input v-model.number="f.fieldNumber" type="number" min="1" placeholder="1" />
                    </td>
                    <td class="c-type">
                      <select v-model="f.type">
                        <option v-for="t in PROTO_TYPES" :key="t" :value="t">{{ t }}</option>
                      </select>
                    </td>
                    <td class="c-val">
                      <input v-model="f.value" :placeholder="protoValuePlaceholder(f.type)" />
                    </td>
                    <td class="c-op">
                      <button class="proto-del" title="删除该字段" @click="removeProtoField(i)">×</button>
                    </td>
                  </tr>
                  <tr v-if="publish.protoFields.length === 0">
                    <td colspan="4" class="empty">暂无字段，点击下方「添加字段」</td>
                  </tr>
                </tbody>
              </table>
              <button class="btn ghost add-field" @click="addProtoField">+ 添加字段</button>
              <div
                v-if="protoPreview"
                class="proto-preview"
                :class="{ 'has-err': protoPreview.startsWith('编码错误') }"
              >
                <span class="pp-label">编码预览</span>
                <code class="pp-hex">{{ protoPreview }}</code>
              </div>
            </div>

            <div class="fmt-hint">
              <span>{{ formatHint }}</span>
              <label
                v-if="publish.format !== 'protobuf'"
                class="auto-seq"
                title="勾选时自动把序号变量填入消息内容"
              >
                <input
                  v-model="publish.autoSeq"
                  type="checkbox"
                  @change="toggleAutoSeq(); saveConfig()"
                />
                自动填充序号递增
              </label>
            </div>
          </div>

          <div class="field inline-fields">
            <div class="sub">
              <label>QoS</label>
              <div class="seg">
                <button v-for="q in [0, 1, 2]" :key="q" class="seg-btn" :class="{ on: publish.qos === q }" @click="publish.qos = q; saveConfig()">
                  {{ q }}
                </button>
              </div>
            </div>
            <div class="sub">
              <label>Retain</label>
              <label class="switch">
                <input v-model="publish.retain" type="checkbox" @change="saveConfig" />
                <span class="slider"></span>
              </label>
            </div>
          </div>

          <div class="field">
            <label>发送方式</label>
            <div class="seg">
              <button class="seg-btn" :class="{ on: publish.mode === 'manual' }" @click="publish.mode = 'manual'; saveConfig()">手动</button>
              <button class="seg-btn" :class="{ on: publish.mode === 'count' }" @click="publish.mode = 'count'; saveConfig()">按数量</button>
              <button class="seg-btn" :class="{ on: publish.mode === 'duration' }" @click="publish.mode = 'duration'; saveConfig()">按时长</button>
            </div>
          </div>

          <div v-if="publish.mode === 'count'" class="field inline-fields">
            <div class="sub">
              <label>数量(包)</label>
              <input v-model.number="publish.count" type="number" min="1" class="num" />
            </div>
            <div class="sub">
              <label>间隔(ms)</label>
              <input v-model.number="publish.intervalMs" type="number" min="1" class="num" />
            </div>
          </div>
          <div v-if="publish.mode === 'duration'" class="field inline-fields">
            <div class="sub">
              <label>时长</label>
              <div class="inline">
                <input v-model.number="publish.durationSec" type="number" min="1" class="num" @change="saveConfig" />
                <select v-model="publish.durationUnit" class="unit-select" @change="saveConfig">
                  <option value="sec">秒</option>
                  <option value="hour">时</option>
                </select>
              </div>
            </div>
            <div class="sub">
              <label>间隔(ms)</label>
              <input v-model.number="publish.intervalMs" type="number" min="1" class="num" @change="saveConfig" />
            </div>
          </div>

          <div class="field">
            <label>超时(ms)</label>
            <input v-model.number="timeoutMs" type="number" min="100" class="num" @change="saveConfig" />
          </div>

          <div v-if="pausedTip" class="paused-tip">{{ pausedTip }}</div>

          <div class="actions">
            <button
              class="btn primary big"
              :disabled="!connected || sendState === 'running'"
              @click="startSend"
            >
              {{ sendState === 'paused' && publish.mode !== 'manual' ? '继续发送' : '开始发送' }}
            </button>
            <button
              class="btn warn big"
              :disabled="sendState !== 'running'"
              @click="stopSend"
            >
              停止发送
            </button>
            <button v-if="sendState === 'paused'" class="btn ghost big" @click="restartSend">
              重新开始
            </button>
          </div>
        </div>
      </section>

      <!-- 右栏：消息记录 -->
      <main class="col-right">
        <div class="msg-head">
          <div class="msg-filters">
            <div class="seg">
              <button class="seg-btn" :class="{ on: filter === 'all' }" @click="filter = 'all'">全部</button>
              <button class="seg-btn" :class="{ on: filter === 'send' }" @click="filter = 'send'">已发送</button>
              <button class="seg-btn" :class="{ on: filter === 'recv' }" @click="filter = 'recv'">已接收</button>
            </div>
            <div class="seg">
              <button class="seg-btn" :class="{ on: statusFilter === 'all' }" @click="statusFilter = 'all'">全部状态</button>
              <button class="seg-btn" :class="{ on: statusFilter === 'success' }" @click="statusFilter = 'success'">成功</button>
              <button class="seg-btn" :class="{ on: statusFilter === 'timeout' }" @click="statusFilter = 'timeout'">超时</button>
              <button class="seg-btn" :class="{ on: statusFilter === 'waiting' }" @click="statusFilter = 'waiting'">等待</button>
            </div>
            <div class="igroup topic-filter">
              <span class="iaddon">TOPIC</span>
              <input v-model="topicSearch" placeholder="按主题过滤" />
            </div>
          </div>
          <button class="btn ghost" @click="messages = []">清空记录</button>
        </div>

        <div class="stats">
          <span class="stat">已发送 <b>{{ stats.sent }}</b></span>
          <span class="stat ok">成功 <b>{{ stats.success }}</b></span>
          <span class="stat err">超时 <b>{{ stats.timeout }}</b></span>
          <span class="stat">等待 <b>{{ stats.waiting }}</b></span>
          <span class="stat err">丢包率 <b>{{ stats.lossRate }}%</b></span>
        </div>

        <div class="table-wrap">
          <table class="msg-table">
            <thead>
              <tr>
                <th class="c-time">时间</th>
                <th class="c-topic">主题</th>
                <th class="c-dir">方向</th>
                <th class="c-qos">QoS</th>
                <th class="c-content">内容</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="m in filteredMessages" :key="m.id" :class="{ 'row-timeout': m.status === 'timeout' }">
                <td class="c-time">{{ m.time }}</td>
                <td class="c-topic" :title="m.topic">{{ m.topic }}</td>
                <td class="c-dir">
                  <span class="dir" :class="m.direction === 'send' ? 'up' : 'down'">
                    {{ m.direction === 'send' ? '发送' : '接收' }}
                  </span>
                  <span v-if="m.status === 'timeout'" class="badge-timeout">超时</span>
                </td>
                <td class="c-qos">{{ m.qos }}</td>
                <td class="c-content" :title="m.content">
                  <span class="content-text" :class="{ 'is-hex': isHexContent(m.content) }" @click="openDetail(m)">{{ m.content }}</span>
                  <button class="copy-btn" title="复制内容" @click="copyMessage(m.content)">⧉</button>
                </td>
              </tr>
              <tr v-if="filteredMessages.length === 0">
                <td colspan="5" class="empty">还没有消息，连接后发送即可在此查看</td>
              </tr>
            </tbody>
          </table>
        </div>
      </main>
    </div>

    <!-- 连接设置弹窗 -->
    <div v-if="showConnModal" class="modal-mask" @click.self="closeConnModal">
      <div class="modal">
        <div class="modal-head">
          <span>连接设置</span>
          <button class="modal-x" @click="closeConnModal">×</button>
        </div>
        <div class="modal-body">
          <div class="field">
            <label>Broker 地址</label>
            <div class="inline">
              <div class="igroup">
                <span class="iaddon">HOST</span>
                <input v-model="connForm.host" placeholder="127.0.0.1" />
              </div>
              <div class="igroup port">
                <span class="iaddon">PORT</span>
                <input v-model.number="connForm.port" type="number" />
              </div>
            </div>
          </div>
          <div class="field">
            <label>用户名</label>
            <div class="igroup">
              <span class="iaddon">USER</span>
              <input v-model="connForm.username" placeholder="可选" />
            </div>
          </div>
          <div class="field">
            <label>密码</label>
            <div class="igroup">
              <span class="iaddon">PASS</span>
              <input v-model="connForm.password" type="password" placeholder="可选" />
            </div>
          </div>
          <div class="field">
            <label>Client ID</label>
            <div class="inline">
              <div class="igroup">
                <span class="iaddon">ID</span>
                <input v-model="connForm.clientId" />
              </div>
              <button class="btn ghost" @click="genClientIdFor(connForm)">生成</button>
            </div>
          </div>
        </div>
        <div class="modal-foot">
          <button class="btn" @click="closeConnModal">取消</button>
          <button class="btn primary" @click="saveConnModal">确定</button>
        </div>
      </div>
    </div>

    <!-- 方案弹窗（新建/重命名） -->
    <div v-if="showProfileModal" class="modal-mask" @click.self="showProfileModal = false">
      <div class="modal">
        <div class="modal-head">
          <span>{{ profileModalMode === 'new' ? '新建方案' : '重命名方案' }}</span>
          <button class="modal-x" @click="showProfileModal = false">×</button>
        </div>
        <div class="modal-body">
          <div class="field">
            <label>方案名称</label>
            <input
              v-model="profileNameInput"
              class="text-input"
              placeholder="新建项目方案名称"
              @keyup.enter="confirmProfileModal"
            />
          </div>
        </div>
        <div class="modal-foot">
          <button class="btn" @click="showProfileModal = false">取消</button>
          <button class="btn primary" @click="confirmProfileModal">确定</button>
        </div>
      </div>
    </div>

    <!-- 删除方案确认弹窗 -->
    <div v-if="showDeleteModal" class="modal-mask" @click.self="showDeleteModal = false">
      <div class="modal">
        <div class="modal-head">
          <span>删除方案</span>
          <button class="modal-x" @click="showDeleteModal = false">×</button>
        </div>
        <div class="modal-body">
          <p class="delete-tip">
            确定要删除方案「<b>{{ activeProfile?.name || '' }}</b>」吗？<br />
            删除后该方案的连接、订阅、发布配置将一并清除，且不可恢复。
          </p>
        </div>
        <div class="modal-foot">
          <button class="btn" @click="showDeleteModal = false">取消</button>
          <button class="btn danger" @click="confirmDelete">确认删除</button>
        </div>
      </div>
    </div>

    <!-- 消息详情弹窗 -->
    <div v-if="detailMsg" class="modal-mask" @click.self="closeDetail">
      <div class="modal detail-modal">
        <div class="modal-head">
          <span>消息详情</span>
          <button class="modal-x" @click="closeDetail">×</button>
        </div>
        <div class="modal-body">
          <div class="detail-meta">
            <span class="dir" :class="detailMsg.direction === 'send' ? 'up' : 'down'">{{ detailMsg.direction === 'send' ? '发送' : '接收' }}</span>
            <span class="detail-topic">{{ detailMsg.topic }}</span>
            <span>QoS {{ detailMsg.qos }}</span>
            <span>{{ detailMsg.time }}</span>
            <span v-if="detailMsg.status === 'timeout'" class="badge-timeout">超时</span>
          </div>
          <pre class="detail-content" :class="{ 'is-hex': isHexContent(detailMsg.content) }">{{ prettyContent(detailMsg.content) }}</pre>
        </div>
        <div class="modal-foot">
          <button class="btn" @click="closeDetail">关闭</button>
          <button class="btn primary" @click="copyMessage(detailMsg.content)">复制内容</button>
        </div>
      </div>
    </div>

    <!-- 订阅主题编辑弹窗 -->
    <div v-if="editingSub != null" class="modal-mask" @click.self="cancelEditSub">
      <div class="modal">
        <div class="modal-head">
          <span>编辑订阅主题</span>
          <button class="modal-x" @click="cancelEditSub">×</button>
        </div>
        <div class="modal-body">
          <div class="field">
            <label>主题（Topic）</label>
            <div class="igroup">
              <span class="iaddon">SUB</span>
              <input v-model="editingSubValue" placeholder="xxx/testDownRes" @keyup.enter="confirmEditSub" />
            </div>
          </div>
        </div>
        <div class="modal-foot">
          <button class="btn" @click="cancelEditSub">取消</button>
          <button class="btn primary" @click="confirmEditSub">确定</button>
        </div>
      </div>
    </div>

    <!-- 错误提示弹窗 -->
    <div v-if="errorMsg" class="modal-mask" @click.self="closeError">
      <div class="modal err-modal">
        <div class="modal-head">
          <h3>错误提示</h3>
          <button class="modal-x" @click="closeError">×</button>
        </div>
        <div class="modal-body err-body">{{ errorMsg }}</div>
        <div class="modal-foot">
          <button class="btn primary" @click="closeError">我知道了</button>
        </div>
      </div>
    </div>
  </div>
</template>

<style>
:root {
  --teal: #0d9488;
  --teal-dark: #0f766e;
  --bg: #f1f5f9;
  --card: #ffffff;
  --border: #e2e8f0;
  --text: #0f172a;
  --muted: #64748b;
  --red: #dc2626;
  --red-bg: #fef2f2;
  --green: #16a34a;
  --amber: #d97706;
}
* { box-sizing: border-box; }
body { margin: 0; }
.app {
  display: flex; flex-direction: column; height: 100vh;
  font-family: -apple-system, "Segoe UI", "Microsoft YaHei", sans-serif;
  background: var(--bg); color: var(--text); font-size: 13px;
}

/* ---------- 顶栏 ---------- */
.topbar {
  display: flex; align-items: center; justify-content: space-between;
  gap: 16px; padding: 12px 18px; background: var(--card); border-bottom: 1px solid var(--border);
}
.brand { display: flex; align-items: center; gap: 10px; flex: none; }
.logo { font-size: 20px; color: var(--teal); }
.title { font-size: 16px; font-weight: 700; }
.subtitle { color: var(--muted); font-size: 12px; }
.profile-bar { display: flex; align-items: center; gap: 8px; flex: 1; justify-content: center; min-width: 0; }
.profile-select {
  min-width: 180px; max-width: 260px; flex: 0 1 260px;
  padding: 6px 10px; border: 1px solid var(--border); border-radius: 6px;
  font-size: 12px; background: #fff; color: var(--text); cursor: pointer;
}
.profile-select:focus { outline: none; border-color: var(--teal); box-shadow: 0 0 0 3px rgba(13,148,136,.12); }
.top-right { display: flex; align-items: center; gap: 12px; flex: none; }
.status-pill {
  padding: 4px 14px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 600;
  max-width: 320px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.status-pill.ok { background: #dcfce7; color: var(--green); }
.status-pill.off { background: #fee2e2; color: var(--red); }
.status-pill.ing { background: #fef3c7; color: var(--amber); }
.btn.soft-off { opacity: 0.4; cursor: not-allowed; }

/* ---------- 三栏布局 ---------- */
.body { display: flex; flex: 1; overflow: hidden; }
.col-left { width: 340px; min-width: 340px; overflow-y: auto; padding: 14px; display: flex; flex-direction: column; gap: 14px; border-right: 1px solid var(--border); }
.col-mid { width: 500px; min-width: 500px; overflow-y: auto; padding: 14px; border-right: 1px solid var(--border); }
.col-right { flex: 1; display: flex; flex-direction: column; padding: 14px; gap: 12px; overflow: hidden; min-width: 0; }

/* ---------- 卡片 ---------- */
.card { background: var(--card); border: 1px solid var(--border); border-radius: 10px; padding: 14px; }
.card h3 { margin: 0 0 14px; font-size: 13px; font-weight: 700; color: var(--teal-dark); display: flex; align-items: center; gap: 6px; }
.hint { color: var(--muted); font-weight: 400; font-size: 11px; }

/* ---------- 连接概览 ---------- */
.overview-card { cursor: pointer; transition: box-shadow .15s, border-color .15s; }
.overview-card:hover { border-color: var(--teal); box-shadow: 0 2px 10px rgba(13,148,136,.14); }
.ov-head { display: flex; align-items: center; gap: 10px; }
.ov-icon {
  width: 34px; height: 34px; border-radius: 8px; flex: none;
  background: linear-gradient(135deg, #0d9488, #14b8a6);
  color: #fff; display: flex; align-items: center; justify-content: center; font-size: 18px;
}
.ov-title { flex: 1; min-width: 0; }
.ov-name { font-size: 14px; font-weight: 700; }
.ov-sub { font-size: 11px; color: var(--muted); margin-top: 1px; }
.ov-status {
  display: flex;
  align-items: center;
  gap: 5px;
  font-size: 12px;
  color: var(--muted);
  white-space: nowrap;
  max-width: 55%;
  overflow: hidden;
  text-overflow: ellipsis;
  flex: none;
}
.ov-status .dot { width: 8px; height: 8px; border-radius: 50%; background: #cbd5e1; flex: none; }
.ov-status.ok { color: var(--green); }
.ov-status.ok .dot { background: var(--green); }
.ov-status.ing { color: var(--amber); }
.ov-status.ing .dot { background: var(--amber); }
.ov-rows { margin-top: 14px; border-top: 1px solid var(--border); }
.ov-row { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; padding: 9px 0; border-bottom: 1px solid #f1f5f9; }
.ov-row span { color: var(--muted); font-size: 12px; flex: none; }
.ov-row b { font-size: 13px; color: var(--text); font-family: Consolas, monospace; max-width: 62%; word-break: break-all; text-align: right; }
.ov-edit { margin-top: 10px; font-size: 11px; color: var(--teal); text-align: center; }

/* ---------- 表单 ---------- */
.field { margin-bottom: 13px; }
.field > label { display: block; color: var(--muted); font-size: 12px; margin-bottom: 6px; }
.field .num { width: 140px; padding: 8px 10px; border: 1px solid var(--border); border-radius: 6px; font-size: 12px; background: #fff; color: var(--text); font-family: Consolas, monospace; transition: border-color .15s, box-shadow .15s; }
.field .num:focus { outline: none; border-color: var(--teal); box-shadow: 0 0 0 3px rgba(13,148,136,.12); }
.unit-select { padding: 8px 6px; border: 1px solid var(--border); border-radius: 6px; font-size: 12px; background: #fff; color: var(--text); cursor: pointer; transition: border-color .15s, box-shadow .15s; }
.unit-select:focus { outline: none; border-color: var(--teal); box-shadow: 0 0 0 3px rgba(13,148,136,.12); }

.inline { display: flex; align-items: center; gap: 8px; }
.inline-fields { display: flex; gap: 24px; align-items: flex-end; }
.inline-fields .sub { display: flex; flex-direction: column; gap: 6px; }
.inline-fields .sub label { margin-bottom: 0; }

.label-row { display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px; }
.label-row label { color: var(--muted); font-size: 12px; }
.fmt-hint {
  margin-top: 7px;
  color: var(--muted);
  font-size: 11px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
}
.auto-seq {
  display: flex;
  align-items: center;
  gap: 4px;
  cursor: pointer;
  white-space: nowrap;
  user-select: none;
}
.auto-seq input {
  accent-color: var(--primary, #14b8a6);
  cursor: pointer;
}

/* ---------- protobuf 字段表格 ---------- */
.proto-mode-row { margin-bottom: 8px; }
.proto-panel {
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow: hidden;
  background: #fff;
}
.proto-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.proto-table thead th {
  text-align: left;
  padding: 7px 8px;
  background: #f8fafc;
  color: var(--muted);
  font-weight: 600;
  border-bottom: 1px solid var(--border);
}
.proto-table td { padding: 5px 6px; border-bottom: 1px solid #f1f5f9; vertical-align: middle; }
.proto-table tbody tr:last-child td { border-bottom: none; }
.proto-table .c-num { width: 86px; }
.proto-table .c-type { width: 104px; }
.proto-table .c-op { width: 36px; text-align: center; }
.proto-table input,
.proto-table select {
  width: 100%;
  padding: 6px 8px;
  border: 1px solid var(--border);
  border-radius: 5px;
  font-size: 12px;
  background: #fff;
  color: var(--text);
  font-family: Consolas, monospace;
}
.proto-table input:focus,
.proto-table select:focus {
  outline: none;
  border-color: var(--teal);
  box-shadow: 0 0 0 3px rgba(13, 148, 136, 0.12);
}
.proto-table .empty { color: var(--muted); text-align: center; padding: 14px 0; font-size: 12px; }
.proto-del {
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  font-size: 16px;
  line-height: 1;
  padding: 0 4px;
}
.proto-del:hover { color: var(--red); }
.proto-panel .add-field { margin: 8px; }

/* ---------- 内嵌标签输入组 ---------- */
.igroup {
  display: flex; align-items: stretch; width: 100%;
  border: 1px solid var(--border); border-radius: 6px; overflow: hidden;
  background: #fff; transition: border-color .15s, box-shadow .15s;
}
.inline .igroup { flex: 1; min-width: 0; width: auto; }
.igroup:focus-within { border-color: var(--teal); box-shadow: 0 0 0 3px rgba(13,148,136,.12); }
.iaddon {
  display: flex; align-items: center; padding: 0 10px;
  background: #f8fafc; color: var(--muted); font-size: 10px; font-weight: 700;
  letter-spacing: .5px; border-right: 1px solid var(--border); white-space: nowrap;
  font-family: Consolas, monospace;
}
.igroup input {
  flex: 1; min-width: 0; border: none; background: transparent;
  padding: 8px 10px; font-size: 12px; outline: none; color: var(--text);
  font-family: Consolas, monospace;
}
.igroup input::placeholder { color: #94a3b8; }
.igroup.port { width: 118px; flex: none; }

/* ---------- 格式切换 tabs（胶囊分段，选中态完整覆盖） ---------- */
.format-tabs {
  display: inline-flex; align-items: stretch; gap: 3px;
  background: #eef2f7; padding: 3px; border-radius: 8px;
}
.format-tabs .tab {
  padding: 5px 14px; border: none; background: transparent; cursor: pointer;
  font-size: 12px; color: var(--muted); border-radius: 6px; font-family: inherit;
  white-space: nowrap; transition: background .15s, color .15s, box-shadow .15s;
}
.format-tabs .tab:hover { color: var(--text); }
.format-tabs .tab.on {
  background: #fff; color: var(--teal); font-weight: 600;
  box-shadow: 0 1px 2px rgba(15,23,42,.10), 0 0 0 1px rgba(13,148,136,.15);
}

/* ---------- 按钮 ---------- */
.btn { padding: 6px 14px; border-radius: 6px; border: 1px solid var(--border); background: #fff; cursor: pointer; font-size: 12px; color: var(--text); white-space: nowrap; }
.btn:hover { background: #f1f5f9; }
.btn:disabled { opacity: 0.5; cursor: not-allowed; pointer-events: none; }
.btn.primary { background: var(--teal); color: #fff; border-color: var(--teal); }
.btn.primary:hover { background: var(--teal-dark); }
.btn.danger { background: var(--red); color: #fff; border-color: var(--red); }
.btn.warn { background: #f59e0b; color: #fff; border-color: #f59e0b; }
.btn.ghost { background: transparent; color: var(--teal-dark); border-color: var(--border); }
.btn.ghost.add { flex: none; }
.btn.big { padding: 9px 24px; font-size: 13px; font-weight: 600; }

/* ---------- 分段选择 ---------- */
.seg { display: inline-flex; border: 1px solid var(--border); border-radius: 6px; overflow: hidden; }
.seg-btn { padding: 5px 14px; border: none; background: #fff; cursor: pointer; font-size: 12px; color: var(--muted); border-right: 1px solid var(--border); }
.seg-btn:last-child { border-right: none; }
.seg-btn.on { background: var(--teal); color: #fff; }

/* ---------- Retain 开关 ---------- */
.switch { position: relative; display: inline-block; width: 40px; height: 22px; margin: 0; cursor: pointer; }
.switch input { opacity: 0; width: 0; height: 0; }
.slider { position: absolute; inset: 0; background: #cbd5e1; border-radius: 999px; transition: .2s; }
.slider::before { content: ''; position: absolute; width: 16px; height: 16px; left: 3px; top: 3px; background: #fff; border-radius: 50%; transition: .2s; }
.switch input:checked + .slider { background: var(--teal); }
.switch input:checked + .slider::before { transform: translateX(18px); }

/* ---------- 订阅列表 ---------- */
.sub-list { list-style: none; margin: 10px 0 0; padding: 0; display: flex; flex-direction: column; gap: 5px; }
.sub-list li { display: flex; align-items: flex-start; gap: 8px; padding: 7px 10px; background: #f8fafc; border: 1px solid var(--border); border-radius: 6px; }
.topic-chip { flex: 1; min-width: 0; font-family: Consolas, monospace; font-size: 12px; word-break: break-all; line-height: 1.5; cursor: pointer; }
.topic-chip:hover { color: var(--teal); text-decoration: underline; }
.x { flex: none; border: none; background: transparent; color: var(--muted); cursor: pointer; font-size: 16px; padding: 0 4px; line-height: 1; }
.x:hover { color: var(--red); }
.sub-list .empty { color: var(--muted); font-size: 12px; text-align: center; padding: 14px 0; }

.actions { display: flex; gap: 10px; margin-top: 6px; }
.paused-tip {
  margin-top: 8px;
  padding: 6px 10px;
  border-radius: 6px;
  background: #fff7ed;
  border: 1px solid #fed7aa;
  color: #b45309;
  font-size: 11px;
  line-height: 1.5;
}

/* ---------- 消息记录 ---------- */
.msg-head { display: flex; align-items: center; justify-content: space-between; }
.stats { display: flex; gap: 20px; padding: 9px 14px; background: var(--card); border: 1px solid var(--border); border-radius: 8px; flex-wrap: wrap; }
.stat { color: var(--muted); font-size: 12px; }
.stat b { color: var(--text); font-size: 15px; margin-left: 2px; }
.stat.ok b { color: var(--green); }
.stat.err b { color: var(--red); }

.table-wrap { flex: 1; overflow: auto; background: var(--card); border: 1px solid var(--border); border-radius: 8px; }
.msg-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.msg-table thead th { position: sticky; top: 0; background: #f8fafc; text-align: left; padding: 8px 10px; color: var(--muted); font-weight: 600; border-bottom: 1px solid var(--border); z-index: 1; }
.msg-table td { padding: 6px 10px; border-bottom: 1px solid #f1f5f9; vertical-align: top; }
.msg-table tbody tr:hover { background: #f8fafc; }
.c-time { width: 100px; font-family: Consolas, monospace; white-space: nowrap; }
.c-topic { max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: Consolas, monospace; color: var(--teal-dark); }
.c-dir { width: 112px; white-space: nowrap; }
.c-qos { width: 44px; text-align: center; }
.c-content { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: Consolas, monospace; }
.dir { display: inline-block; white-space: nowrap; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; }
.dir.up { background: #dbeafe; color: #2563eb; }
.dir.down { background: #dcfce7; color: var(--green); }
.badge-timeout { display: inline-block; margin-left: 4px; padding: 1px 6px; border-radius: 3px; background: var(--red); color: #fff; font-size: 10px; font-weight: 700; vertical-align: middle; }
.row-timeout td { background: var(--red-bg); }
.row-timeout .c-time, .row-timeout .c-content { color: var(--red); }
.row-timeout .c-topic { color: var(--red); }

/* ---------- 弹窗 ---------- */
.modal-mask {
  position: fixed; inset: 0; background: rgba(15,23,42,.45);
  display: flex; align-items: center; justify-content: center; z-index: 100;
}
.modal {
  width: 420px; max-width: 92vw; background: #fff; border-radius: 12px;
  box-shadow: 0 10px 40px rgba(0,0,0,.2); overflow: hidden;
}
.modal-head {
  display: flex; align-items: center; justify-content: space-between;
  padding: 14px 18px; border-bottom: 1px solid var(--border);
  font-size: 14px; font-weight: 700;
}
.modal-x { border: none; background: transparent; font-size: 20px; color: var(--muted); cursor: pointer; line-height: 1; }
.modal-x:hover { color: var(--text); }
.modal-body { padding: 18px; }
.modal-foot {
  display: flex; justify-content: flex-end; gap: 10px;
  padding: 14px 18px; border-top: 1px solid var(--border);
}
.err-modal { width: 460px; max-width: 90vw; }
.err-body {
  color: var(--red);
  background: var(--red-bg);
  border-radius: 6px;
  padding: 10px 12px;
  font-size: 12px;
  line-height: 1.6;
  word-break: break-all;
  user-select: text;
}
.text-input {
  width: 100%; padding: 8px 10px; border: 1px solid var(--border); border-radius: 6px;
  font-size: 13px; background: #fff; color: var(--text);
}
.text-input:focus { outline: none; border-color: var(--teal); box-shadow: 0 0 0 3px rgba(13,148,136,.12); }
.delete-tip { font-size: 13px; line-height: 1.7; color: var(--text); margin: 0; }
.delete-tip b { color: var(--red); }

/* ---------- 断线醒目化 ---------- */
.overview-card.conn-broken {
  border-color: var(--red);
  background: var(--red-bg);
}
.overview-card.conn-broken .ov-name { color: var(--red); }

/* ---------- 消息记录增强 ---------- */
.msg-filters {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: 1;
  min-width: 0;
  flex-wrap: wrap;
}
.msg-head { gap: 10px; }
.topic-filter { width: 180px; flex: none; }
.copy-btn {
  flex: none;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  font-size: 13px;
  line-height: 1;
  padding: 0 2px;
  margin-left: 4px;
  vertical-align: middle;
  opacity: 0;
}
.msg-table tr:hover .copy-btn { opacity: 1; }
.copy-btn:hover { color: var(--teal); }
.content-text { cursor: pointer; }
.content-text:hover { color: var(--teal); text-decoration: underline; }
.content-text.is-hex { color: #7c3aed; }

/* ---------- 消息详情弹窗 ---------- */
.detail-modal { width: 560px; max-width: 92vw; }
.detail-meta {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 10px;
  font-size: 12px;
  color: var(--muted);
}
.detail-topic {
  font-family: Consolas, monospace;
  color: var(--teal-dark);
  word-break: break-all;
}
.detail-content {
  margin: 0;
  padding: 12px;
  background: #f8fafc;
  border: 1px solid var(--border);
  border-radius: 6px;
  font-family: Consolas, monospace;
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
  max-height: 420px;
  overflow: auto;
  user-select: text;
}
.detail-content.is-hex { color: #7c3aed; }

/* ---------- protobuf 编码预览 ---------- */
.proto-preview {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  margin: 8px;
  padding: 8px 10px;
  background: #f0fdfa;
  border: 1px solid #99f6e4;
  border-radius: 6px;
}
.proto-preview.has-err {
  background: var(--red-bg);
  border-color: #fecaca;
}
.pp-label {
  flex: none;
  font-size: 11px;
  font-weight: 600;
  color: var(--teal-dark);
  padding-top: 1px;
}
.proto-preview.has-err .pp-label { color: var(--red); }
.pp-hex {
  flex: 1;
  min-width: 0;
  font-family: Consolas, monospace;
  font-size: 11px;
  word-break: break-all;
  line-height: 1.6;
  color: var(--text);
  user-select: text;
}
.proto-preview.has-err .pp-hex { color: var(--red); }
</style>
