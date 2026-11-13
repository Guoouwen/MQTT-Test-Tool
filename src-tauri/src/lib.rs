mod mqtt;

use mqtt::{u8_to_qos, MqttManager};
use serde::Deserialize;
use std::sync::Mutex;
use tauri::State;

/// 全局应用状态
struct AppState {
    mqtt: Mutex<MqttManager>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
    client_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishConfig {
    topic: String,
    payload: String,
    qos: u8,
    retain: bool,
    #[serde(default = "default_format")]
    format: String,
}

fn default_format() -> String {
    "json".into()
}

/// 将 hex 字符串解码为字节（protobuf 二进制），非法时退回按 UTF-8 文本
fn decode_hex(s: &str) -> Vec<u8> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    let cleaned = cleaned.trim_start_matches("0x").trim_start_matches("0X");
    if cleaned.is_empty() || cleaned.len() % 2 != 0 {
        return s.as_bytes().to_vec();
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    let bytes = cleaned.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16);
        let lo = (bytes[i + 1] as char).to_digit(16);
        match (hi, lo) {
            (Some(h), Some(l)) => out.push(((h as u8) << 4) | (l as u8)),
            _ => return s.as_bytes().to_vec(),
        }
        i += 2;
    }
    out
}

/// 连接 broker
#[tauri::command]
async fn connect(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    config: ConnectConfig,
) -> Result<(), String> {
    let mut mqtt = state.mqtt.lock().map_err(|e| e.to_string())?;
    mqtt.connect(
        &config.host,
        config.port,
        &config.username,
        &config.password,
        &config.client_id,
        app,
    )
}

/// 断开连接
#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    let mut mqtt = state.mqtt.lock().map_err(|e| e.to_string())?;
    mqtt.disconnect();
    Ok(())
}

/// 订阅主题
#[tauri::command]
async fn subscribe(state: State<'_, AppState>, topic: String, qos: u8) -> Result<(), String> {
    let client = {
        let mqtt = state.mqtt.lock().map_err(|e| e.to_string())?;
        mqtt.client.clone().ok_or("尚未连接 broker")?
    };
    client
        .subscribe(topic, u8_to_qos(qos))
        .await
        .map_err(|e| e.to_string())
}

/// 取消订阅
#[tauri::command]
async fn unsubscribe(state: State<'_, AppState>, topic: String) -> Result<(), String> {
    let client = {
        let mqtt = state.mqtt.lock().map_err(|e| e.to_string())?;
        mqtt.client.clone().ok_or("尚未连接 broker")?
    };
    client.unsubscribe(topic).await.map_err(|e| e.to_string())
}

/// 发布消息
#[tauri::command]
async fn publish(state: State<'_, AppState>, config: PublishConfig) -> Result<(), String> {
    let client = {
        let mqtt = state.mqtt.lock().map_err(|e| e.to_string())?;
        mqtt.client.clone().ok_or("尚未连接 broker")?
    };
    let bytes = if config.format == "protobuf" {
        decode_hex(&config.payload)
    } else {
        config.payload.into_bytes()
    };
    client
        .publish(
            config.topic,
            u8_to_qos(config.qos),
            config.retain,
            bytes,
        )
        .await
        .map_err(|e| e.to_string())
}

/// 查询连接状态
#[tauri::command]
async fn get_status(state: State<'_, AppState>) -> Result<bool, String> {
    let mqtt = state.mqtt.lock().map_err(|e| e.to_string())?;
    Ok(mqtt.is_connected())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            mqtt: Mutex::new(MqttManager::new()),
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            disconnect,
            subscribe,
            unsubscribe,
            publish,
            get_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
