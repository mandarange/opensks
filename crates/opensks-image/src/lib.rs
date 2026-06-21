use opensks_contracts::{
    IMAGE_ASSET_SCHEMA, IMAGE_LEDGER_SCHEMA, ImageAsset, ImageLedger, RoutingStatus, VisualAnchor,
};
use opensks_provider::{ModelRegistry, RoutingRequest};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("no enabled compatible image model")]
    MissingImageModel,
    #[error("visual anchor escapes image bounds")]
    AnchorOutOfBounds,
}

#[derive(Debug, Clone)]
pub struct ImageRuntime {
    ledger: ImageLedger,
}

impl ImageRuntime {
    pub fn new() -> Self {
        Self {
            ledger: ImageLedger {
                schema: IMAGE_LEDGER_SCHEMA.to_string(),
                assets: Vec::new(),
                gc_candidate_ids: Vec::new(),
            },
        }
    }

    pub fn generate_placeholder(
        &mut self,
        registry: &ModelRegistry,
        id: impl Into<String>,
        width: u32,
        height: u32,
        anchors: Vec<VisualAnchor>,
    ) -> Result<ImageAsset, ImageError> {
        let decision = registry.route(&RoutingRequest::for_image("image-route"));
        if decision.status != RoutingStatus::Routed {
            return Err(ImageError::MissingImageModel);
        }
        validate_anchors(width, height, &anchors)?;
        let id = id.into();
        let asset = ImageAsset {
            schema: IMAGE_ASSET_SCHEMA.to_string(),
            content_hash: stable_hash(format!("{id}:{width}:{height}").as_bytes()),
            id: id.clone(),
            provider_id: "fake-local".to_string(),
            model_id: decision.selected_model_id.unwrap_or_default(),
            path: format!(".opensks/assets/candidates/{id}.png"),
            width,
            height,
            before_asset_id: None,
            anchors,
            temporary: true,
            evidence_refs: vec!["opensks-image:placeholder-ledger".to_string()],
        };
        self.ledger.gc_candidate_ids.push(id);
        self.ledger.assets.push(asset.clone());
        Ok(asset)
    }

    pub fn relate_before_after(
        &mut self,
        before_id: &str,
        mut after: ImageAsset,
    ) -> Result<ImageAsset, ImageError> {
        validate_anchors(after.width, after.height, &after.anchors)?;
        after.before_asset_id = Some(before_id.to_string());
        self.ledger.assets.push(after.clone());
        Ok(after)
    }

    pub fn ledger(&self) -> &ImageLedger {
        &self.ledger
    }
}

impl Default for ImageRuntime {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_anchors(width: u32, height: u32, anchors: &[VisualAnchor]) -> Result<(), ImageError> {
    for anchor in anchors {
        if anchor.x.saturating_add(anchor.width) > width
            || anchor.y.saturating_add(anchor.height) > height
        {
            return Err(ImageError::AnchorOutOfBounds);
        }
    }
    Ok(())
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensks_policy::PermissionPolicy;

    #[test]
    fn one_enabled_image_model_fallback_is_used() {
        let registry = ModelRegistry::new(
            vec![
                opensks_provider::fake_image_model("disabled-image", false),
                opensks_provider::fake_image_model("enabled-image", true),
            ],
            PermissionPolicy::default(),
        );
        let mut runtime = ImageRuntime::new();
        let asset = runtime
            .generate_placeholder(&registry, "asset-1", 512, 512, Vec::new())
            .expect("image asset");
        assert_eq!(asset.model_id, "enabled-image");
        assert!(
            runtime
                .ledger()
                .gc_candidate_ids
                .contains(&"asset-1".to_string())
        );
    }

    #[test]
    fn disabled_image_model_is_never_called() {
        let registry = ModelRegistry::new(
            vec![opensks_provider::fake_image_model("disabled-image", false)],
            PermissionPolicy::default(),
        );
        let mut runtime = ImageRuntime::new();
        let error = runtime
            .generate_placeholder(&registry, "asset-1", 512, 512, Vec::new())
            .expect_err("missing model");
        assert!(matches!(error, ImageError::MissingImageModel));
    }

    #[test]
    fn anchor_bounds_and_before_after_relation_are_checked() {
        let registry = ModelRegistry::new(
            vec![opensks_provider::fake_image_model("enabled-image", true)],
            PermissionPolicy::default(),
        );
        let mut runtime = ImageRuntime::new();
        let before = runtime
            .generate_placeholder(
                &registry,
                "before",
                100,
                100,
                vec![VisualAnchor {
                    x: 10,
                    y: 10,
                    width: 20,
                    height: 20,
                }],
            )
            .expect("before");
        let mut after = before.clone();
        after.id = "after".to_string();
        let related = runtime
            .relate_before_after("before", after)
            .expect("relation");
        assert_eq!(related.before_asset_id.as_deref(), Some("before"));
        let mut bad = before.clone();
        bad.anchors = vec![VisualAnchor {
            x: 90,
            y: 90,
            width: 20,
            height: 20,
        }];
        assert!(matches!(
            runtime.relate_before_after("before", bad),
            Err(ImageError::AnchorOutOfBounds)
        ));
    }
}
