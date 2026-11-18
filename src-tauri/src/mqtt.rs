use rumqttc::v5::{AsyncClient, Event, Incoming, MqttOptions};
use rumqttc::v5::mqttbytes::QoS;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

/// 推送给前端的收到消息结构
#[derive(Serialize, Clone)]
pub struct IncomingMessage {
    pub topic: String,
    pub payload: String,
    pub qos: u8,
    pub timestamp: u64,
}

/// 连接状态事件结构
#[derive(Serialize, Clone)]
pub struct StatusEvent {
    pub connected: bool,
    pub message: String,
}

/// MQTT 客户端管理器：持有 AsyncClient，后台线程跑事件循环
pub struct MqttManager {
    pub client: Option<AsyncClient>,
    running: Arc<AtomicBool>,
}

impl Default for MqttManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MqttManager {
    pub fn new() -> Self {
        Self {
            client: None,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 建立连接并启动事件循环线程
    pub fn connect(
        &mut self,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        client_id: &str,
        app: AppHandle,
    ) -> Result<(), String> {
        let mut options = MqttOptions::new(client_id, host, port);
        // rumqttc::v5 模块即 MQTT 5.0 协议
        options.set_keep_alive(std::time::Duration::from_secs(30));
        if !username.is_empty() {
            options.set_credentials(username, password);
        }

        let (client, mut eventloop) = AsyncClient::new(options, 100);
        self.client = Some(client);
        self.running.store(true, Ordering::SeqCst);

        let running = self.running.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            rt.block_on(async move {
                // 首次连接成功后再断线才自动重连；首次连接失败直接报错，避免填错地址后无限重试。
                let mut connected_once = false;
                loop {
                    if !running.load(Ordering::SeqCst) {
                        break;
                    }
                    match eventloop.poll().await {
                        Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                            connected_once = true;
                            let _ = app.emit(
                                "mqtt-status",
                                StatusEvent {
                                    connected: true,
                                    message: "已连接".into(),
                                },
                            );
                        }
                        Ok(Event::Incoming(Incoming::Publish(p))) => {
                            // 二进制 payload（如 protobuf 回执）转 hex（0x 前缀），前端据此区分文本与二进制
                            let payload = match String::from_utf8(p.payload.to_vec()) {
                                Ok(s) => s,
                                Err(_) => {
                                    let hex: String =
                                        p.payload.iter().map(|b| format!("{b:02x}")).collect();
                                    format!("0x{hex}")
                                }
                            };
                            let msg = IncomingMessage {
                                topic: String::from_utf8_lossy(&p.topic).to_string(),
                                payload,
                                qos: qos_to_u8(p.qos),
                                timestamp: now_ms(),
                            };
                            let _ = app.emit("mqtt-message", msg);
                        }
                        Ok(Event::Incoming(Incoming::Disconnect(_))) => {
                            let _ = app.emit(
                                "mqtt-status",
                                StatusEvent {
                                    connected: false,
                                    message: "已断开".into(),
                                },
                            );
                            if !connected_once || !running.load(Ordering::SeqCst) {
                                break;
                            }
                            let _ = app.emit(
                                "mqtt-status",
                                StatusEvent {
                                    connected: false,
                                    message: "重连中...".into(),
                                },
                            );
                            // rumqttc 持续 poll 即自动重连；稍等后继续循环触发重连
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                        Err(e) => {
                            if !connected_once || !running.load(Ordering::SeqCst) {
                                let _ = app.emit(
                                    "mqtt-status",
                                    StatusEvent {
                                        connected: false,
                                        message: format!("连接错误: {e}"),
                                    },
                                );
                                break;
                            }
                            let _ = app.emit(
                                "mqtt-status",
                                StatusEvent {
                                    connected: false,
                                    message: format!("重连中... {e}"),
                                },
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                        _ => {}
                    }
                }
            });
        });

        Ok(())
    }

    /// 断开连接，停止事件循环
    pub fn disconnect(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(client) = &self.client {
            let _ = client.try_disconnect();
        }
        self.client = None;
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }
}

fn qos_to_u8(q: QoS) -> u8 {
    match q {
        QoS::AtMostOnce => 0,
        QoS::AtLeastOnce => 1,
        QoS::ExactlyOnce => 2,
    }
}

pub fn u8_to_qos(q: u8) -> QoS {
    match q {
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtMostOnce,
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
