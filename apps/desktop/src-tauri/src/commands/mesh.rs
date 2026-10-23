//! Nexus Mesh 命令 (去中心化对等网络与住宅出口通道共享)。

use crate::state::AppState;
use nexus_core::Result;
use nexus_mesh::{HostChannel, MarketNode, MeshConfig, MeshPeer, MeshStatus, PairingCodeInfo};
use tauri::State;

#[tauri::command]
pub async fn mesh_get_status(state: State<'_, AppState>) -> Result<MeshStatus> {
    Ok(state.mesh.get_status().await)
}

#[tauri::command]
pub async fn mesh_set_sharing(state: State<'_, AppState>, enabled: bool) -> Result<MeshStatus> {
    state.mesh.set_sharing_enabled(enabled).await;
    Ok(state.mesh.get_status().await)
}

#[tauri::command]
pub async fn mesh_update_config(
    state: State<'_, AppState>,
    config: MeshConfig,
) -> Result<MeshStatus> {
    state.mesh.update_config(config).await;
    Ok(state.mesh.get_status().await)
}

#[tauri::command]
pub async fn mesh_list_market_nodes(state: State<'_, AppState>) -> Result<Vec<MarketNode>> {
    Ok(state.mesh.list_market_nodes().await)
}

#[tauri::command]
pub async fn mesh_generate_pairing_code(state: State<'_, AppState>) -> Result<PairingCodeInfo> {
    Ok(state.mesh.generate_pairing_code().await)
}

#[tauri::command]
pub async fn mesh_pair_device(
    state: State<'_, AppState>,
    code: String,
    peer_name: Option<String>,
) -> Result<MeshPeer> {
    state
        .mesh
        .pair_device(&code, peer_name)
        .await
        .map_err(nexus_core::AppError::invalid)
}

#[tauri::command]
pub async fn mesh_unpair_device(state: State<'_, AppState>, peer_node_id: String) -> Result<bool> {
    Ok(state.mesh.unpair_device(&peer_node_id).await)
}

#[tauri::command]
pub async fn mesh_update_peer_channels(
    state: State<'_, AppState>,
    peer_node_id: String,
    shared_channels: Vec<String>,
) -> Result<bool> {
    Ok(state
        .mesh
        .update_peer_channels(&peer_node_id, shared_channels)
        .await)
}

#[tauri::command]
pub async fn mesh_update_host_channel(
    state: State<'_, AppState>,
    channel: HostChannel,
) -> Result<bool> {
    Ok(state.mesh.update_host_channel(channel).await)
}

#[tauri::command]
pub async fn mesh_update_device_name(
    state: State<'_, AppState>,
    name: String,
) -> Result<MeshStatus> {
    state.mesh.update_device_name(name).await;
    Ok(state.mesh.get_status().await)
}
