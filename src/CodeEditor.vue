<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref, watch } from 'vue';
import {
  EditorView,
  keymap,
  lineNumbers,
  highlightActiveLine,
  highlightActiveLineGutter,
  drawSelection,
  dropCursor,
} from '@codemirror/view';
import { EditorState, Compartment } from '@codemirror/state';
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
import {
  syntaxHighlighting,
  defaultHighlightStyle,
  indentOnInput,
  bracketMatching,
} from '@codemirror/language';
import { json } from '@codemirror/lang-json';

const props = defineProps<{
  modelValue: string;
  format: 'json' | 'protobuf' | 'plaintext';
}>();
const emit = defineEmits<{ (e: 'update:modelValue', v: string): void }>();

const host = ref<HTMLDivElement>();
let view: EditorView | null = null;
const langCompartment = new Compartment();

function languageFor(f: string) {
  return f === 'json' ? json() : [];
}

function buildState(doc: string, format: string) {
  return EditorState.create({
    doc,
    extensions: [
      lineNumbers(),
      highlightActiveLineGutter(),
      highlightActiveLine(),
      history(),
      drawSelection(),
      dropCursor(),
      indentOnInput(),
      bracketMatching(),
      EditorState.allowMultipleSelections.of(true),
      syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
      keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
      EditorView.lineWrapping,
      langCompartment.of(languageFor(format)),
      EditorView.updateListener.of((u) => {
        if (u.docChanged) emit('update:modelValue', u.state.doc.toString());
      }),
      EditorView.theme({
        '&': { fontSize: '13px' },
        '.cm-scroller': { fontFamily: 'Consolas, "Courier New", monospace' },
        '.cm-gutters': {
          backgroundColor: '#f8fafc',
          color: '#94a3b8',
          borderRight: '1px solid #e2e8f0',
        },
        '.cm-activeLineGutter': { backgroundColor: '#eef2f7' },
        '.cm-activeLine': { backgroundColor: '#f8fafc' },
        '.cm-content': { caretColor: '#0f172a' },
      }),
    ],
  });
}

onMounted(() => {
  view = new EditorView({
    state: buildState(props.modelValue, props.format),
    parent: host.value!,
  });
});

watch(
  () => props.modelValue,
  (v) => {
    if (view && v !== view.state.doc.toString()) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } });
    }
  },
);

watch(
  () => props.format,
  (f) => {
    if (view) {
      view.dispatch({ effects: langCompartment.reconfigure(languageFor(f)) });
    }
  },
);

onBeforeUnmount(() => view?.destroy());
</script>

<template>
  <div ref="host" class="code-editor"></div>
</template>

<style>
.code-editor {
  border: 1px solid #e2e8f0;
  border-radius: 8px;
  overflow: hidden;
  background: #fff;
  transition: border-color 0.15s, box-shadow 0.15s;
}
.code-editor:focus-within {
  border-color: #0d9488;
  box-shadow: 0 0 0 3px rgba(13, 148, 136, 0.12);
}
.code-editor .cm-editor {
  height: 240px;
}
.code-editor .cm-editor.cm-focused {
  outline: none;
}
</style>
