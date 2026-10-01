use crate::McpError;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rsi_media_protocol::{ImageImportOptions, MAXIMUM_IMAGE_DESCRIPTOR_BYTES, Media};
use rsi_tools_protocol::{ToolContent, ToolResult};
use serde_json::{Value, json};

// Rich durable content is independent of the model used for any particular Step.
pub(crate) async fn result(
    media: &dyn Media,
    server: &str,
    raw: Value,
    mut metadata: Value,
    items: Vec<Value>,
    is_error: bool,
) -> ToolResult {
    metadata["result"] = raw;
    if items.len() > 256 {
        return crate::contribution::unavailable(server, McpError::Capacity);
    }
    let mut remaining = MAXIMUM_IMAGE_DESCRIPTOR_BYTES;
    let mut content = Vec::with_capacity(items.len());
    let mut projection_failed = false;
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                let Some(text) = item.get("text").and_then(Value::as_str) else {
                    return crate::contribution::unavailable(server, McpError::Protocol);
                };
                content.push(ToolContent::Text { text: text.into() });
            }
            Some("image") => {
                let image = async {
                    let data = item.get("data")?.as_str()?;
                    let mime = item.get("mimeType")?.as_str()?;
                    if remaining == 0 {
                        return None;
                    }
                    let bytes = STANDARD.decode(data).ok()?;
                    media
                        .import_image_with_options(
                            bytes.into(),
                            ImageImportOptions {
                                source_mime: Some(mime.into()),
                                maximum_output_bytes: remaining,
                            },
                        )
                        .await
                        .ok()
                }
                .await;
                if let Some(image) = image.filter(|image| image.bytes <= remaining) {
                    remaining -= image.bytes;
                    content.push(ToolContent::Image { media: image });
                } else {
                    projection_failed = true;
                    content.push(image_failure());
                }
            }
            Some("audio" | "resource" | "resource_link") => content.push(ToolContent::Text {
                text: format!(
                    "MCP {server}: {} content is retained in the structured result",
                    item["type"].as_str().expect("matched type")
                ),
            }),
            _ => return crate::contribution::unavailable(server, McpError::Protocol),
        }
    }
    if projection_failed {
        // No partial image batch is emitted. Earlier immutable CAS imports may remain.
        for item in &mut content {
            if matches!(item, ToolContent::Image { .. }) {
                *item = image_failure();
            }
        }
        metadata["projection_error"] = json!("image_import_failed");
    }
    ToolResult::new(metadata, content, is_error || projection_failed)
        .unwrap_or_else(|_| crate::contribution::unavailable(server, McpError::Capacity))
}

fn image_failure() -> ToolContent {
    ToolContent::Text { text: "MCP image could not be validated or durably imported within the result's image budget; original content is retained in the structured result.".into() }
}

pub(crate) async fn tool_result(
    media: &dyn Media,
    server: &str,
    tool: &str,
    value: Value,
) -> ToolResult {
    let Some(items) = value
        .get("content")
        .and_then(Value::as_array)
        .filter(|items| items.len() <= 256)
    else {
        return crate::contribution::unavailable(server, McpError::Protocol);
    };
    let is_error = match value.get("isError") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return crate::contribution::unavailable(server, McpError::Protocol),
    };
    let items = items.iter().map(expand_embedded_resource).collect();
    result(
        media,
        server,
        value,
        json!({"version":1,"server":server,"tool":tool}),
        items,
        is_error,
    )
    .await
}

fn expand_embedded_resource(item: &Value) -> Value {
    if item.get("type").and_then(Value::as_str) == Some("resource")
        && let Some(resource) = item.get("resource")
    {
        return resource_content(resource);
    }
    item.clone()
}

pub(crate) fn resource_content(resource: &Value) -> Value {
    if let Some(text) = resource.get("text").and_then(Value::as_str) {
        json!({"type":"text","text":text})
    } else if resource
        .get("mimeType")
        .and_then(Value::as_str)
        .is_some_and(|mime| mime.starts_with("image/"))
    {
        json!({"type":"image","data":resource.get("blob"),"mimeType":resource.get("mimeType")})
    } else {
        json!({"type":"resource"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_media_protocol::MediaContract;
    use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
    use std::{io::Cursor, sync::Arc};

    #[tokio::test]
    async fn images_are_durable_ordered_and_a_failed_batch_retains_raw_without_partial_images() {
        let runtime = Runtime::default();
        let backend = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "memory",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(rsi_media_testkit::MemoryMediaBackendFactory),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let service = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "media",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(rsi_media::MediaFactory),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let media = runtime.root().lookup_local::<MediaContract>().unwrap();
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            2,
            3,
            image::Rgba([1, 2, 3, 255]),
        ))
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        let encoded = STANDARD.encode(&png);
        let value = json!({"content":[{"type":"text","text":"before"},{"type":"image","mimeType":"image/png","data":encoded},{"type":"text","text":"after"}],"structuredContent":{"exact":18_446_744_073_709_551_615_u64},"isError":true});
        let result = tool_result(media.as_ref(), "fixture", "image", value.clone()).await;
        result.validate().unwrap();
        assert_eq!(result.value["result"], value);
        assert!(result.is_error);
        assert!(matches!(&result.content[0], ToolContent::Text { text } if text == "before"));
        assert!(matches!(&result.content[2], ToolContent::Text { text } if text == "after"));
        let ToolContent::Image { media: reference } = &result.content[1] else {
            panic!("image was not imported")
        };
        assert_eq!((reference.width, reference.height), (2, 3));
        let stored = media.read(reference).await.unwrap();
        assert_eq!(stored.bytes.len() as u64, reference.bytes);

        let embedded = json!({"content":[{"type":"resource","resource":{"uri":"fixture://picture","mimeType":"image/png","blob":encoded}}]});
        let imported = tool_result(media.as_ref(), "fixture", "image", embedded.clone()).await;
        assert_eq!(imported.value["result"], embedded);
        assert_eq!(
            imported.content,
            vec![ToolContent::Image {
                media: reference.clone()
            }]
        );

        for invalid in [
            json!({"type":"image","mimeType":"image/jpeg","data":encoded}),
            json!({"type":"image","mimeType":"image/png","data":"AB=="}),
            json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(b"not an image")}),
        ] {
            let raw = json!({"content":[value["content"][0],value["content"][1],invalid,value["content"][2]]});
            let failed = tool_result(media.as_ref(), "fixture", "image", raw.clone()).await;
            failed.validate().unwrap();
            assert!(failed.is_error);
            assert_eq!(failed.value["result"], raw);
            assert_eq!(failed.value["projection_error"], "image_import_failed");
            assert!(
                !failed
                    .content
                    .iter()
                    .any(|item| matches!(item, ToolContent::Image { .. }))
            );
            assert_eq!(failed.content.len(), 4);
        }
        // The already published object remains readable after a partial import failure.
        media.read(reference).await.unwrap();
        drop(media);
        assert!(service.dispose().await.is_clean());
        assert!(backend.dispose().await.is_clean());
    }
    #[derive(Debug, Default)]
    struct BudgetMedia(std::sync::Mutex<Vec<u64>>, bool);
    #[async_trait::async_trait]
    impl Media for BudgetMedia {
        async fn import_image_with_options(
            &self,
            _: bytes::Bytes,
            options: ImageImportOptions,
        ) -> rsi_media_protocol::Result<rsi_media_protocol::MediaRef> {
            let bytes = MAXIMUM_IMAGE_DESCRIPTOR_BYTES / 2 + 1;
            self.0.lock().unwrap().push(options.maximum_output_bytes);
            if bytes > options.maximum_output_bytes && !self.1 {
                return Err(rsi_media_protocol::MediaError::Codec(
                    "output budget".into(),
                ));
            }
            Ok(rsi_media_protocol::MediaRef {
                id: rsi_media_protocol::MediaId::new("a".repeat(64)).unwrap(),
                mime: "image/png".into(),
                width: 1,
                height: 1,
                bytes,
            })
        }
        async fn read(
            &self,
            _: &rsi_media_protocol::MediaRef,
        ) -> rsi_media_protocol::Result<rsi_media_protocol::StoredMedia> {
            panic!("normalization does not reread imports")
        }
    }
    #[tokio::test]
    async fn second_image_receives_only_the_remaining_budget_and_failure_removes_all_image_positions()
     {
        let media = BudgetMedia::default();
        let image =
            json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(b"fixture")});
        let raw = json!({"content":[image.clone(),image]});
        let result = tool_result(&media, "fixture", "image", raw.clone()).await;
        assert!(result.is_error);
        assert_eq!(result.value["result"], raw);
        assert_eq!(
            *media.0.lock().unwrap(),
            vec![
                MAXIMUM_IMAGE_DESCRIPTOR_BYTES,
                MAXIMUM_IMAGE_DESCRIPTOR_BYTES / 2 - 1
            ]
        );
        assert_eq!(result.content.len(), 2);
        assert!(
            result
                .content
                .iter()
                .all(|item| matches!(item, ToolContent::Text { .. }))
        );
    }

    #[tokio::test]
    async fn over_budget_import_response_rejects_the_complete_image_projection() {
        let media = BudgetMedia(std::sync::Mutex::new(vec![]), true);
        let image =
            json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(b"fixture")});
        let raw = json!({"content":[image.clone(),image]});
        let result = tool_result(&media, "fixture", "image", raw.clone()).await;
        result.validate().unwrap();
        assert_eq!(media.0.lock().unwrap().len(), 2);
        assert!(result.is_error);
        assert_eq!(result.value["result"], raw);
        assert_eq!(result.value["projection_error"], "image_import_failed");
        assert!(
            result
                .content
                .iter()
                .all(|item| matches!(item, ToolContent::Text { .. }))
        );
    }
}
