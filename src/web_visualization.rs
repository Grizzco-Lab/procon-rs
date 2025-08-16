//! Web-based Pro Controller visualization
//!
//! Provides a web server with real-time WebSocket updates showing
//! controller state in a browser. No GPU dependencies!

use crate::keystate::ControllerState;
use crate::parser::ProConParser;
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tokio::time::{Duration, interval};
use warp::Filter;
use warp::ws::{Message, WebSocket};

/// Web visualization server
pub struct WebVisualizationServer {
    controller_state: Arc<Mutex<Option<ControllerState>>>,
    broadcaster: broadcast::Sender<String>,
    is_device_connected: Arc<Mutex<bool>>,
}

impl WebVisualizationServer {
    /// Create a new web visualization server
    pub fn new() -> Self {
        let (broadcaster, _) = broadcast::channel(100);

        Self {
            controller_state: Arc::new(Mutex::new(None)),
            broadcaster,
            is_device_connected: Arc::new(Mutex::new(false)), // Default to disconnected until data arrives
        }
    }

    /// Update controller state from HID data
    pub async fn update_from_hid(&self, data: &[u8]) {
        // Update device connection status to connected
        *self.is_device_connected.lock().await = true;

        if data.len() > 0 && data[0] == 0x30 {
            if let Ok(state) = ProConParser::parse_input_report(data) {
                *self.controller_state.lock().await = Some(state.clone());

                // Broadcast to all connected clients
                let json_data = self.serialize_controller_state(&state);
                let _ = self.broadcaster.send(json_data);
            }
        }
    }

    /// Update device connection status
    pub async fn update_device_status(&self, connected: bool) {
        let current_status = *self.is_device_connected.lock().await;
        if current_status != connected {
            *self.is_device_connected.lock().await = connected;

            // Broadcast connection status update
            let status_data = json!({
                "type": "device_status",
                "connected": connected,
                "timestamp": chrono::Utc::now().timestamp_millis()
            })
            .to_string();

            let _ = self.broadcaster.send(status_data);
        }
    }

    /// Serialize controller state to JSON
    fn serialize_controller_state(&self, state: &ControllerState) -> String {
        json!({
            "type": "controller_state",
            "timestamp": state.timestamp.timestamp_millis(),
            "buttons": {
                "a": state.buttons.a,
                "b": state.buttons.b,
                "x": state.buttons.x,
                "y": state.buttons.y,
                "up": state.buttons.up,
                "down": state.buttons.down,
                "left": state.buttons.left,
                "right": state.buttons.right,
                "l": state.buttons.l,
                "r": state.buttons.r,
                "zl": state.buttons.zl,
                "zr": state.buttons.zr,
                "minus": state.buttons.minus,
                "plus": state.buttons.plus,
                "home": state.buttons.home,
                "capture": state.buttons.capture,
                "l_stick": state.buttons.l_stick,
                "r_stick": state.buttons.r_stick
            },
            "sticks": {
                "left": {
                    "x": state.left_stick.x,
                    "y": state.left_stick.y
                },
                "right": {
                    "x": state.right_stick.x,
                    "y": state.right_stick.y
                }
            },
            "gyro": state.gyro.iter().map(|g| json!({
                "gyro_x": g.gyro_x,
                "gyro_y": g.gyro_y,
                "gyro_z": g.gyro_z
            })).collect::<Vec<_>>(),
            "battery": state.battery_level
        })
        .to_string()
    }

    /// Start the web server
    pub async fn start_server(&self, port: u16) -> Result<()> {
        let controller_state = Arc::clone(&self.controller_state);
        let broadcaster = self.broadcaster.clone();
        let is_device_connected = Arc::clone(&self.is_device_connected);

        // Static HTML page
        let html_page = self.get_html_page();
        let html_route = warp::path::end()
            .and(warp::get())
            .map(move || warp::reply::html(html_page.clone()));

        // WebSocket route
        let websocket_route = warp::path("ws")
            .and(warp::ws())
            .and(warp::any().map(move || broadcaster.clone()))
            .and(warp::any().map(move || Arc::clone(&controller_state)))
            .and(warp::any().map(move || Arc::clone(&is_device_connected)))
            .map(|ws: warp::ws::Ws, broadcaster, state, device_status| {
                ws.on_upgrade(move |websocket| handle_websocket(websocket, broadcaster, state, device_status))
            });

        let routes = html_route.or(websocket_route);

        log::info!(
            "Starting web visualization server on http://0.0.0.0:{}",
            port
        );
        log::info!("Open http://your-ip:{} in your browser", port);

        warp::serve(routes).run(([0, 0, 0, 0], port)).await;

        Ok(())
    }

    /// Get the embedded HTML page
    fn get_html_page(&self) -> String {
        include_str!("../web/gamepad.html").to_string()
    }
}

/// Handle WebSocket connection
async fn handle_websocket(
    websocket: WebSocket,
    broadcaster: broadcast::Sender<String>,
    controller_state: Arc<Mutex<Option<ControllerState>>>,
    is_device_connected: Arc<Mutex<bool>>,
) {
    let (mut ws_tx, mut ws_rx) = websocket.split();
    let mut rx = broadcaster.subscribe();

    // Send initial state if available
    if let Some(state) = &*controller_state.lock().await {
        let json_data = json!({
            "type": "controller_state",
            "timestamp": state.timestamp.timestamp_millis(),
            "buttons": {
                "a": state.buttons.a,
                "b": state.buttons.b,
                "x": state.buttons.x,
                "y": state.buttons.y,
                "up": state.buttons.up,
                "down": state.buttons.down,
                "left": state.buttons.left,
                "right": state.buttons.right,
                "l": state.buttons.l,
                "r": state.buttons.r,
                "zl": state.buttons.zl,
                "zr": state.buttons.zr,
                "minus": state.buttons.minus,
                "plus": state.buttons.plus,
                "home": state.buttons.home,
                "capture": state.buttons.capture,
                "l_stick": state.buttons.l_stick,
                "r_stick": state.buttons.r_stick
            },
            "sticks": {
                "left": {
                    "x": state.left_stick.x,
                    "y": state.left_stick.y
                },
                "right": {
                    "x": state.right_stick.x,
                    "y": state.right_stick.y
                }
            },
            "gyro": state.gyro.iter().map(|g| json!({
                "gyro_x": g.gyro_x,
                "gyro_y": g.gyro_y,
                "gyro_z": g.gyro_z
            })).collect::<Vec<_>>(),
            "battery": state.battery_level
        })
        .to_string();

        let _ = ws_tx.send(Message::text(json_data)).await;
    }

    // Send current device status immediately
    let connected = *is_device_connected.lock().await;
    let status_data = json!({
        "type": "device_status",
        "connected": connected,
        "timestamp": chrono::Utc::now().timestamp_millis()
    })
    .to_string();
    let _ = ws_tx.send(Message::text(status_data)).await;

    // Handle messages
    let mut ping_interval = interval(Duration::from_secs(30));

    loop {
        tokio::select! {
            // Forward broadcast messages to WebSocket
            msg = rx.recv() => {
                match msg {
                    Ok(data) => {
                        if ws_tx.send(Message::text(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            // Handle incoming WebSocket messages
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(msg)) => {
                        if msg.is_close() {
                            break;
                        }
                        // Echo ping/pong for keep-alive
                        if msg.is_ping() {
                            let _ = ws_tx.send(Message::pong(msg.into_bytes())).await;
                        }
                    }
                    _ => break,
                }
            }

            // Send periodic ping
            _ = ping_interval.tick() => {
                if ws_tx.send(Message::ping(vec![])).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Web visualization dumper that works with existing dumper system
#[derive(Clone)]
pub struct WebVisualizationDumper {
    server: Arc<WebVisualizationServer>,
    last_data_time: Arc<Mutex<std::time::Instant>>,
}

impl WebVisualizationDumper {
    /// Create a new web visualization dumper
    pub fn new() -> (Self, Arc<WebVisualizationServer>) {
        let server = Arc::new(WebVisualizationServer::new());
        let dumper = Self {
            server: Arc::clone(&server),
            last_data_time: Arc::new(Mutex::new(std::time::Instant::now())),
        };

        (dumper, server)
    }

    /// Update device connection status
    pub fn update_device_status(&self, connected: bool) {
        let server = Arc::clone(&self.server);

        // Use existing runtime or create task appropriately
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                server.update_device_status(connected).await;
            });
        } else {
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    server.update_device_status(connected).await;
                });
            });
        }
    }

    /// Check if device should be considered connected based on recent data
    pub fn is_device_connected(&self) -> bool {
        if let Ok(last_time) = self.last_data_time.try_lock() {
            let elapsed = last_time.elapsed();
            elapsed < std::time::Duration::from_secs(3) // Consider disconnected if no data for 3 seconds
        } else {
            true // Default to connected if we can't check
        }
    }
}

impl crate::dump::Dumper for WebVisualizationDumper {
    fn dump(&mut self, data: &[u8]) -> Result<()> {
        // Update last data time
        if let Ok(mut last_time) = self.last_data_time.try_lock() {
            *last_time = std::time::Instant::now();
        }

        // Try to get a tokio runtime handle, if available
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let server = Arc::clone(&self.server);
            let data = data.to_vec();

            handle.spawn(async move {
                server.update_from_hid(&data).await;
            });
        } else {
            // If no runtime available, create a blocking task
            let server = Arc::clone(&self.server);
            let data = data.to_vec();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    server.update_from_hid(&data).await;
                });
            });
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        // No-op for web visualization
        Ok(())
    }
}
