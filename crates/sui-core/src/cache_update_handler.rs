use dashmap::DashSet;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use sui_types::base_types::ObjectID;
use sui_types::object::Object;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use tracing::{error, info, debug, warn};

const SOCKET_PATH: &str = "/tmp/sui_cache_updates.sock";
pub const POOL_RELATED_OBJECTS_PATH: &str = "/home/kakaxi/config/pool_related_ids.txt";

pub fn pool_related_object_ids() -> DashSet<ObjectID> {
    let content = std::fs::read_to_string(POOL_RELATED_OBJECTS_PATH)
        .unwrap_or_else(|_| panic!("Failed to open: {}", POOL_RELATED_OBJECTS_PATH));

    let set = DashSet::new();
    content
        .trim()
        .split('\n')
        .map(|line| line.parse().expect("Failed to parse pool_related_ids"))
        .for_each(|id| {
            set.insert(id);
        });
    set
}

#[derive(Debug, Clone)]
pub struct CacheUpdateHandler {
    socket_path: PathBuf,     
    connections: Arc<Mutex<Vec<UnixStream>>>,
    running: Arc<AtomicBool>,
}

impl CacheUpdateHandler {
    pub fn new() -> Self {
        let socket_path = PathBuf::from(SOCKET_PATH);
        // Remove existing socket file if it exists
        let _ = std::fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path).expect("Failed to bind Unix socket");
        info!("CacheUpdateHandler: listening on socket {:?}", socket_path);

        let connections = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(true));

        let connections_clone = Arc::clone(&connections);
        let running_clone = Arc::clone(&running);

        // Spawn connection acceptor task
        tokio::spawn(async move {
            while running_clone.load(Ordering::SeqCst) {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        info!("New client connected to cache update socket");
                        let mut connections = connections_clone.lock().await;
                        connections.push(stream);
                    }
                    Err(e) => {
                        error!("Error accepting connection: {}", e);
                        // Optionally, decide whether to break the loop or continue
                    }
                }
            }
        });

        Self {
            socket_path,
            connections,
            running,
        }
    }

    pub async fn notify_written(&self, objects: Vec<(ObjectID, Object)>) {
        debug!("CacheUpdateHandler: notify_written called with {} objects", objects.len());
        let start_time = std::time::Instant::now();

        let serialized = bcs::to_bytes(&objects).expect("serialization error");
        let len = serialized.len() as u32;
        let len_bytes = len.to_le_bytes();

        debug!("CacheUpdateHandler: acquiring connections lock...");
        let mut connections = self.connections.lock().await;
        debug!("CacheUpdateHandler: got connections lock, {} active connections", connections.len());

        // Iterate over connections and remove any that fail
        let mut i = 0;
        while i < connections.len() {
            let stream = &mut connections[i];

            let result = async {
                if let Err(e) = stream.write_all(&len_bytes).await {
                    error!("CacheUpdateHandler: error writing length prefix: {}", e);
                    Err(e)
                } else if let Err(e) = stream.write_all(&serialized).await {
                    error!("CacheUpdateHandler: error writing payload: {}", e);
                    Err(e)
                } else {
                    Ok(())
                }
            }.await;

            if result.is_err() {
                warn!("CacheUpdateHandler: removing a dead connection");
                connections.remove(i);
            } else {
                i += 1;
            }
        }

        debug!("CacheUpdateHandler: notify_written completed in {:?}", start_time.elapsed());
    }
}

impl Default for CacheUpdateHandler {
    fn default() -> Self {
        Self::new()
    }
}

// impl Drop for CacheUpdateHandler {
//     fn drop(&mut self) {
//         self.running.store(false, Ordering::SeqCst);
//         let _ = std::fs::remove_file(&self.socket_path);
//     }
// }
