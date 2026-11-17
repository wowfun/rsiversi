pub async fn assert_assets_unavailable(result: rsi_host::Result<rsi_host::RunningHost>) {
    let running = result.expect("a rejected leaf leaves a degraded Profile");
    let status = running.profile_status();
    assert_eq!(status.health(), rsi_host::ProfileHealth::Degraded);
    assert!(status.diagnostic().is_some());
    assert!(
        running
            .lookup_local::<rsi_api_http::HttpAssetsContract>()
            .is_none()
    );
    assert!(
        running
            .lookup_local::<rsi_web_assets::WebAssetControlContract>()
            .is_none()
    );
    assert!(running.shutdown().await.is_clean());
}
