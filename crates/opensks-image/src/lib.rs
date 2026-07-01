use std::{
    fs,
    io::Write,
    path::{Component, Path},
};

use opensks_contracts::{
    IMAGE_ASSET_SCHEMA, IMAGE_LEDGER_SCHEMA, IMAGE_PROVENANCE_RECEIPT_SCHEMA, ImageAsset,
    ImageLedger, ImageOperation, ImageProvenanceReceipt, ModelRouteReceipt, VisualAnchor,
};
use opensks_provider::{ModelRegistry, RoutingRequest};
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_MOCK_IMAGE_PIXELS: u64 = 16_777_216;

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("no enabled compatible image model")]
    MissingImageModel,
    #[error("resolved image route did not include provider/model provenance")]
    MissingRouteReceipt,
    #[error("image asset not found")]
    AssetNotFound,
    #[error("invalid image asset id")]
    InvalidAssetId,
    #[error("invalid image asset path")]
    InvalidAssetPath,
    #[error("invalid image dimensions")]
    InvalidDimensions,
    #[error("image content hash mismatch: expected {expected}, actual {actual}")]
    ContentHashMismatch { expected: String, actual: String },
    #[error("image asset id `{id}` already exists with different content: expected {expected}, actual {actual}")]
    AssetIdConflict {
        id: String,
        expected: String,
        actual: String,
    },
    #[error("image provider generation failed: {0}")]
    Provider(String),
    #[error("image generation requires a non-empty prompt")]
    MissingPrompt,
    #[error("visual anchor escapes image bounds")]
    AnchorOutOfBounds,
    #[error("image artifact io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct ImageRuntime {
    ledger: ImageLedger,
}

#[derive(Debug, Clone)]
pub struct ImageAssetRequest<'a> {
    pub id: &'a str,
    pub width: u32,
    pub height: u32,
    pub anchors: Vec<VisualAnchor>,
    pub prompt: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct ImageProviderRequest<'a> {
    pub provider_id: &'a str,
    pub model_id: &'a str,
    pub remote_model_id: &'a str,
    pub prompt: &'a str,
    pub width: u32,
    pub height: u32,
    pub route_receipt: &'a ModelRouteReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageProviderOutput {
    pub bytes: Vec<u8>,
    pub extension: String,
    pub mime_type: Option<String>,
    pub evidence_refs: Vec<String>,
}

pub trait ImageGenerationClient {
    fn generate_image(
        &self,
        request: &ImageProviderRequest<'_>,
    ) -> Result<ImageProviderOutput, ImageError>;
}

#[derive(Debug, Clone)]
pub struct ImageInspectionProviderRequest<'a> {
    pub provider_id: &'a str,
    pub model_id: &'a str,
    pub remote_model_id: &'a str,
    pub asset_id: &'a str,
    pub content_hash: &'a str,
    pub mime_type: &'a str,
    pub bytes: &'a [u8],
    pub prompt: &'a str,
    pub route_receipt: &'a ModelRouteReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInspectionProviderOutput {
    pub text: String,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInspectionResult {
    pub receipt: ImageProvenanceReceipt,
    pub text: String,
    pub evidence_refs: Vec<String>,
}

pub trait ImageInspectionClient {
    fn inspect_image(
        &self,
        request: &ImageInspectionProviderRequest<'_>,
    ) -> Result<ImageInspectionProviderOutput, ImageError>;
}

impl ImageRuntime {
    pub fn new() -> Self {
        Self {
            ledger: ImageLedger {
                schema: IMAGE_LEDGER_SCHEMA.to_string(),
                assets: Vec::new(),
                provenance_receipts: Vec::new(),
                gc_candidate_ids: Vec::new(),
            },
        }
    }

    pub fn from_ledger(ledger: ImageLedger) -> Self {
        Self { ledger }
    }

    pub fn generate_asset(
        &mut self,
        registry: &ModelRegistry,
        id: impl Into<String>,
        width: u32,
        height: u32,
        anchors: Vec<VisualAnchor>,
        prompt: Option<&str>,
    ) -> Result<ImageAsset, ImageError> {
        let (asset, receipt, _) =
            build_generated_asset(registry, id, width, height, anchors, prompt)?;
        self.record_generated_asset(asset, receipt)
    }

    pub fn generate_asset_file(
        &mut self,
        registry: &ModelRegistry,
        workspace: &Path,
        request: ImageAssetRequest<'_>,
    ) -> Result<ImageAsset, ImageError> {
        let (mut asset, mut receipt, bytes) = build_generated_asset(
            registry,
            request.id,
            request.width,
            request.height,
            request.anchors,
            request.prompt,
        )?;
        write_and_verify_asset_bytes(workspace, &asset, &bytes)?;
        asset
            .evidence_refs
            .push("opensks-image:asset-file-verified".to_string());
        receipt
            .evidence_refs
            .push("opensks-image:asset-file-verified".to_string());
        self.record_generated_asset(asset, receipt)
    }

    pub fn generate_provider_asset_file(
        &mut self,
        registry: &ModelRegistry,
        workspace: &Path,
        request: ImageAssetRequest<'_>,
        client: &dyn ImageGenerationClient,
    ) -> Result<ImageAsset, ImageError> {
        self.generate_provider_asset_file_for_model(registry, workspace, request, None, client)
    }

    pub fn generate_provider_asset_file_for_model(
        &mut self,
        registry: &ModelRegistry,
        workspace: &Path,
        request: ImageAssetRequest<'_>,
        explicit_model_id: Option<&str>,
        client: &dyn ImageGenerationClient,
    ) -> Result<ImageAsset, ImageError> {
        let prompt = request
            .prompt
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(ImageError::MissingPrompt)?;
        let mut routing_request = RoutingRequest::for_image("image-generate");
        routing_request.explicit_model_id = explicit_model_id.map(str::to_string);
        let route_receipt = route_receipt(registry, routing_request)?;
        validate_anchors(request.width, request.height, &request.anchors)?;
        let id = request.id.to_string();
        let provider_id = route_receipt
            .provider_id
            .clone()
            .ok_or(ImageError::MissingRouteReceipt)?;
        let model_id = route_receipt
            .model_id
            .clone()
            .ok_or(ImageError::MissingRouteReceipt)?;
        let remote_model_id = remote_model_id_for_route(registry, &provider_id, &model_id)?;
        let output = client.generate_image(&ImageProviderRequest {
            provider_id: &provider_id,
            model_id: &model_id,
            remote_model_id: &remote_model_id,
            prompt,
            width: request.width,
            height: request.height,
            route_receipt: &route_receipt,
        })?;
        let (mut asset, mut receipt, bytes) = build_generated_asset_from_bytes(
            id,
            request.width,
            request.height,
            request.anchors,
            Some(prompt),
            route_receipt,
            output.bytes,
            &output.extension,
            output.evidence_refs,
        )?;
        write_and_verify_asset_bytes(workspace, &asset, &bytes)?;
        asset
            .evidence_refs
            .push("opensks-image:provider-asset-file-verified".to_string());
        receipt
            .evidence_refs
            .push("opensks-image:provider-asset-file-verified".to_string());
        self.record_generated_asset(asset, receipt)
    }

    fn record_generated_asset(
        &mut self,
        asset: ImageAsset,
        receipt: ImageProvenanceReceipt,
    ) -> Result<ImageAsset, ImageError> {
        if let Some(existing) = self
            .ledger
            .assets
            .iter()
            .find(|existing| existing.id == asset.id)
        {
            if existing.content_hash == asset.content_hash {
                // Idempotent retry: same id, same content. Do not duplicate ledger
                // entries; still record the new provenance receipt (retry is auditable)
                // and return the existing asset unchanged.
                self.ledger.provenance_receipts.push(receipt);
                return Ok(existing.clone());
            }
            return Err(ImageError::AssetIdConflict {
                id: asset.id.clone(),
                expected: existing.content_hash.clone(),
                actual: asset.content_hash.clone(),
            });
        }
        let id = asset.id.clone();
        self.ledger.provenance_receipts.push(receipt);
        self.ledger.gc_candidate_ids.push(id);
        self.ledger.assets.push(asset.clone());
        Ok(asset)
    }

    pub fn inspect_asset(
        &mut self,
        registry: &ModelRegistry,
        asset_id: &str,
        prompt: Option<&str>,
    ) -> Result<ImageProvenanceReceipt, ImageError> {
        let asset = self
            .ledger
            .assets
            .iter()
            .find(|asset| asset.id == asset_id)
            .cloned()
            .ok_or(ImageError::AssetNotFound)?;
        let route_receipt = route_receipt(registry, RoutingRequest::for_vision("image-inspect"))?;
        let prompt_hash = prompt.map(|value| hash_bytes(value.as_bytes()));
        let provider_id = route_receipt
            .provider_id
            .clone()
            .ok_or(ImageError::MissingRouteReceipt)?;
        let model_id = route_receipt
            .model_id
            .clone()
            .ok_or(ImageError::MissingRouteReceipt)?;
        let provenance_hash = provenance_hash(
            &asset.id,
            ImageOperation::Inspect,
            &asset.content_hash,
            prompt_hash.as_deref(),
            &route_receipt,
        );
        let receipt = ImageProvenanceReceipt {
            schema: IMAGE_PROVENANCE_RECEIPT_SCHEMA.to_string(),
            asset_id: asset.id,
            operation: ImageOperation::Inspect,
            provider_id,
            model_id,
            content_hash: asset.content_hash,
            prompt_hash,
            provenance_hash,
            route_receipt,
            evidence_refs: vec!["opensks-image:image.inspect".to_string()],
        };
        self.ledger.provenance_receipts.push(receipt.clone());
        Ok(receipt)
    }

    pub fn inspect_provider_asset_file(
        &mut self,
        registry: &ModelRegistry,
        workspace: &Path,
        artifact_ref: &str,
        prompt: Option<&str>,
        client: &dyn ImageInspectionClient,
    ) -> Result<ImageInspectionResult, ImageError> {
        let asset = self.asset_for_ref(artifact_ref)?;
        let (bytes, mime_type) = read_and_verify_asset_file(workspace, &asset)?;
        let route_receipt = route_receipt(registry, RoutingRequest::for_vision("image-inspect"))?;
        let provider_id = route_receipt
            .provider_id
            .clone()
            .ok_or(ImageError::MissingRouteReceipt)?;
        let model_id = route_receipt
            .model_id
            .clone()
            .ok_or(ImageError::MissingRouteReceipt)?;
        let remote_model_id = remote_model_id_for_route(registry, &provider_id, &model_id)?;
        let effective_prompt = prompt
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Inspect this image artifact and describe the relevant visual content.");
        let output = client.inspect_image(&ImageInspectionProviderRequest {
            provider_id: &provider_id,
            model_id: &model_id,
            remote_model_id: &remote_model_id,
            asset_id: &asset.id,
            content_hash: &asset.content_hash,
            mime_type: &mime_type,
            bytes: &bytes,
            prompt: effective_prompt,
            route_receipt: &route_receipt,
        })?;
        let prompt_hash = Some(hash_bytes(effective_prompt.as_bytes()));
        let provenance_hash = provenance_hash(
            &asset.id,
            ImageOperation::Inspect,
            &asset.content_hash,
            prompt_hash.as_deref(),
            &route_receipt,
        );
        let mut evidence_refs = vec![
            "opensks-image:image.inspect".to_string(),
            "opensks-image:provider-image.inspect".to_string(),
            "opensks-image:asset-file-verified".to_string(),
        ];
        evidence_refs.extend(output.evidence_refs.clone());
        let receipt = ImageProvenanceReceipt {
            schema: IMAGE_PROVENANCE_RECEIPT_SCHEMA.to_string(),
            asset_id: asset.id,
            operation: ImageOperation::Inspect,
            provider_id,
            model_id,
            content_hash: asset.content_hash,
            prompt_hash,
            provenance_hash,
            route_receipt,
            evidence_refs: evidence_refs.clone(),
        };
        self.ledger.provenance_receipts.push(receipt.clone());
        Ok(ImageInspectionResult {
            receipt,
            text: output.text,
            evidence_refs,
        })
    }

    pub fn relate_before_after(
        &mut self,
        before_id: &str,
        mut after: ImageAsset,
    ) -> Result<ImageAsset, ImageError> {
        self.ledger
            .assets
            .iter()
            .find(|asset| asset.id == before_id)
            .ok_or(ImageError::AssetNotFound)?;
        validate_anchors(after.width, after.height, &after.anchors)?;
        after.before_asset_id = Some(before_id.to_string());
        self.ledger.assets.push(after.clone());
        Ok(after)
    }

    pub fn ledger(&self) -> &ImageLedger {
        &self.ledger
    }

    fn asset_for_ref(&self, artifact_ref: &str) -> Result<ImageAsset, ImageError> {
        let reference = artifact_ref.trim();
        let normalized = reference
            .strip_prefix("artifact://")
            .or_else(|| reference.strip_prefix("asset://"))
            .unwrap_or(reference)
            .trim_start_matches("./");
        self.ledger
            .assets
            .iter()
            .find(|asset| {
                let asset_path = asset.path.trim_start_matches("./");
                asset.id == reference
                    || asset.id == normalized
                    || asset.path == reference
                    || asset_path == normalized
            })
            .cloned()
            .ok_or(ImageError::AssetNotFound)
    }
}

impl Default for ImageRuntime {
    fn default() -> Self {
        Self::new()
    }
}

fn build_generated_asset(
    registry: &ModelRegistry,
    id: impl Into<String>,
    width: u32,
    height: u32,
    anchors: Vec<VisualAnchor>,
    prompt: Option<&str>,
) -> Result<(ImageAsset, ImageProvenanceReceipt, Vec<u8>), ImageError> {
    let route_receipt = route_receipt(registry, RoutingRequest::for_image("image-generate"))?;
    validate_anchors(width, height, &anchors)?;
    let id = id.into();
    let file_name = safe_asset_file_name(&id)?;
    let prompt_hash = prompt.map(|value| hash_bytes(value.as_bytes()));
    let provider_id = route_receipt
        .provider_id
        .clone()
        .ok_or(ImageError::MissingRouteReceipt)?;
    let model_id = route_receipt
        .model_id
        .clone()
        .ok_or(ImageError::MissingRouteReceipt)?;
    let bytes = deterministic_ppm_bytes(
        width,
        height,
        &provider_id,
        &model_id,
        &id,
        prompt_hash.as_deref(),
    )?;
    let content_hash = hash_bytes(&bytes);
    let provenance_hash = provenance_hash(
        &id,
        ImageOperation::Generate,
        &content_hash,
        prompt_hash.as_deref(),
        &route_receipt,
    );
    let asset = ImageAsset {
        schema: IMAGE_ASSET_SCHEMA.to_string(),
        content_hash: content_hash.clone(),
        id: id.clone(),
        provider_id: provider_id.clone(),
        model_id: model_id.clone(),
        path: format!(".opensks/assets/candidates/{file_name}.ppm"),
        width,
        height,
        before_asset_id: None,
        anchors,
        temporary: true,
        provenance_hash: Some(provenance_hash.clone()),
        route_receipt: Some(route_receipt.clone()),
        evidence_refs: vec![
            "opensks-image:image.generate".to_string(),
            format!("opensks-image:provenance:{provenance_hash}"),
        ],
    };
    let receipt = ImageProvenanceReceipt {
        schema: IMAGE_PROVENANCE_RECEIPT_SCHEMA.to_string(),
        asset_id: id,
        operation: ImageOperation::Generate,
        provider_id,
        model_id,
        content_hash,
        prompt_hash,
        provenance_hash,
        route_receipt,
        evidence_refs: vec!["opensks-image:image.generate".to_string()],
    };
    Ok((asset, receipt, bytes))
}

#[allow(clippy::too_many_arguments)]
fn build_generated_asset_from_bytes(
    id: impl Into<String>,
    width: u32,
    height: u32,
    anchors: Vec<VisualAnchor>,
    prompt: Option<&str>,
    route_receipt: ModelRouteReceipt,
    bytes: Vec<u8>,
    extension: &str,
    mut evidence_refs: Vec<String>,
) -> Result<(ImageAsset, ImageProvenanceReceipt, Vec<u8>), ImageError> {
    validate_anchors(width, height, &anchors)?;
    let id = id.into();
    let file_name = safe_asset_file_name(&id)?;
    let extension = safe_asset_extension(extension)?;
    let prompt_hash = prompt.map(|value| hash_bytes(value.as_bytes()));
    let provider_id = route_receipt
        .provider_id
        .clone()
        .ok_or(ImageError::MissingRouteReceipt)?;
    let model_id = route_receipt
        .model_id
        .clone()
        .ok_or(ImageError::MissingRouteReceipt)?;
    let content_hash = hash_bytes(&bytes);
    let provenance_hash = provenance_hash(
        &id,
        ImageOperation::Generate,
        &content_hash,
        prompt_hash.as_deref(),
        &route_receipt,
    );
    evidence_refs.insert(0, "opensks-image:provider-image.generate".to_string());
    let mut asset_evidence = vec![
        "opensks-image:image.generate".to_string(),
        format!("opensks-image:provenance:{provenance_hash}"),
    ];
    asset_evidence.extend(evidence_refs.clone());
    let asset = ImageAsset {
        schema: IMAGE_ASSET_SCHEMA.to_string(),
        content_hash: content_hash.clone(),
        id: id.clone(),
        provider_id: provider_id.clone(),
        model_id: model_id.clone(),
        path: format!(".opensks/assets/candidates/{file_name}.{extension}"),
        width,
        height,
        before_asset_id: None,
        anchors,
        temporary: true,
        provenance_hash: Some(provenance_hash.clone()),
        route_receipt: Some(route_receipt.clone()),
        evidence_refs: asset_evidence,
    };
    let receipt = ImageProvenanceReceipt {
        schema: IMAGE_PROVENANCE_RECEIPT_SCHEMA.to_string(),
        asset_id: id,
        operation: ImageOperation::Generate,
        provider_id,
        model_id,
        content_hash,
        prompt_hash,
        provenance_hash,
        route_receipt,
        evidence_refs,
    };
    Ok((asset, receipt, bytes))
}

fn write_and_verify_asset_bytes(
    workspace: &Path,
    asset: &ImageAsset,
    bytes: &[u8],
) -> Result<(), ImageError> {
    let relative_path = Path::new(&asset.path);
    validate_relative_asset_path(relative_path)?;
    let output_path = workspace.join(relative_path);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = output_path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, &output_path)?;
    let written = fs::read(&output_path)?;
    let actual = hash_bytes(&written);
    if actual != asset.content_hash {
        return Err(ImageError::ContentHashMismatch {
            expected: asset.content_hash.clone(),
            actual,
        });
    }
    Ok(())
}

fn read_and_verify_asset_file(
    workspace: &Path,
    asset: &ImageAsset,
) -> Result<(Vec<u8>, String), ImageError> {
    let relative_path = Path::new(&asset.path);
    validate_relative_asset_path(relative_path)?;
    let bytes = fs::read(workspace.join(relative_path))?;
    let actual = hash_bytes(&bytes);
    if actual != asset.content_hash {
        return Err(ImageError::ContentHashMismatch {
            expected: asset.content_hash.clone(),
            actual,
        });
    }
    let mime_type = image_mime_type_for(&bytes, relative_path);
    Ok((bytes, mime_type))
}

fn image_mime_type_for(bytes: &[u8], path: &Path) -> String {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png".to_string()
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg".to_string()
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "image/webp".to_string()
    } else {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("png") => "image/png".to_string(),
            Some("jpg") | Some("jpeg") => "image/jpeg".to_string(),
            Some("webp") => "image/webp".to_string(),
            Some("ppm") => "image/x-portable-pixmap".to_string(),
            _ => "application/octet-stream".to_string(),
        }
    }
}

fn remote_model_id_for_route(
    registry: &ModelRegistry,
    provider_id: &str,
    model_id: &str,
) -> Result<String, ImageError> {
    let model = registry
        .models()
        .iter()
        .find(|model| model.id == model_id)
        .ok_or(ImageError::MissingRouteReceipt)?;
    let prefix = format!("provider:{provider_id}:model:");
    if model.config_ref.source == "provider-registry" {
        return model
            .config_ref
            .reference
            .strip_prefix(&prefix)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .ok_or(ImageError::MissingRouteReceipt);
    }
    Ok(model.id.clone())
}

fn route_receipt(
    registry: &ModelRegistry,
    request: RoutingRequest,
) -> Result<ModelRouteReceipt, ImageError> {
    let decision = registry.route(&request);
    if !decision.status.has_resolved_model() {
        return Err(ImageError::MissingImageModel);
    }
    decision
        .route_receipt
        .ok_or(ImageError::MissingRouteReceipt)
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

fn deterministic_ppm_bytes(
    width: u32,
    height: u32,
    provider_id: &str,
    model_id: &str,
    asset_id: &str,
    prompt_hash: Option<&str>,
) -> Result<Vec<u8>, ImageError> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || pixels > MAX_MOCK_IMAGE_PIXELS {
        return Err(ImageError::InvalidDimensions);
    }
    let seed = hash_bytes(
        format!(
            "{}:{}:{}:{}:{}:{}",
            provider_id,
            model_id,
            asset_id,
            width,
            height,
            prompt_hash.unwrap_or("prompt:none")
        )
        .as_bytes(),
    );
    let mut bytes = format!("P6\n{width} {height}\n255\n").into_bytes();
    let pixel_bytes = (pixels * 3) as usize;
    bytes.reserve(pixel_bytes);
    let seed_bytes = seed.as_bytes();
    for index in 0..pixel_bytes {
        let seed_byte = seed_bytes[index % seed_bytes.len()];
        bytes.push(seed_byte ^ (index as u8).wrapping_mul(31));
    }
    Ok(bytes)
}

fn safe_asset_file_name(id: &str) -> Result<String, ImageError> {
    let trimmed = id.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(ImageError::InvalidAssetId);
    }
    Ok(trimmed.to_string())
}

fn safe_asset_extension(extension: &str) -> Result<String, ImageError> {
    let trimmed = extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return Err(ImageError::InvalidAssetPath);
    }
    Ok(trimmed)
}

fn validate_relative_asset_path(path: &Path) -> Result<(), ImageError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ImageError::InvalidAssetPath);
    }
    Ok(())
}

fn provenance_hash(
    asset_id: &str,
    operation: ImageOperation,
    content_hash: &str,
    prompt_hash: Option<&str>,
    route_receipt: &ModelRouteReceipt,
) -> String {
    let route_json = serde_json::to_string(route_receipt).unwrap_or_default();
    hash_bytes(
        format!(
            "{}:{:?}:{}:{}:{}",
            asset_id,
            operation,
            content_hash,
            prompt_hash.unwrap_or("prompt:none"),
            route_json
        )
        .as_bytes(),
    )
}

fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("write hex");
    }
    format!("sha256:v1:{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensks_policy::PermissionPolicy;
    use std::cell::RefCell;

    #[test]
    fn one_enabled_image_model_fallback_is_used_with_provenance() {
        let registry = ModelRegistry::new(
            vec![
                opensks_provider::fake_image_model("disabled-image", false),
                opensks_provider::fake_image_model("enabled-image", true),
            ],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let mut runtime = ImageRuntime::new();
        let asset = runtime
            .generate_asset(
                &registry,
                "asset-1",
                512,
                512,
                Vec::new(),
                Some("render a test image"),
            )
            .expect("image asset");
        assert_eq!(asset.model_id, "enabled-image");
        assert_eq!(asset.provider_id, "fake-local");
        assert_eq!(asset.path, ".opensks/assets/candidates/asset-1.ppm");
        assert!(asset.content_hash.starts_with("sha256:v1:"));
        assert!(asset.route_receipt.is_some());
        assert!(asset.provenance_hash.is_some());
        assert!(
            asset
                .evidence_refs
                .contains(&"opensks-image:image.generate".to_string())
        );
        assert_eq!(runtime.ledger().provenance_receipts.len(), 1);
        let receipt = &runtime.ledger().provenance_receipts[0];
        assert_eq!(receipt.asset_id, "asset-1");
        assert_eq!(receipt.model_id, "enabled-image");
        assert_eq!(receipt.provider_id, "fake-local");
        assert_eq!(receipt.content_hash, asset.content_hash);
        assert!(receipt.prompt_hash.is_some());
        assert_eq!(receipt.provenance_hash, asset.provenance_hash.unwrap());
        assert!(
            runtime
                .ledger()
                .gc_candidate_ids
                .contains(&"asset-1".to_string())
        );
    }

    #[test]
    fn generated_image_file_is_written_and_hash_verified() {
        let registry = ModelRegistry::new(
            vec![opensks_provider::fake_image_model("enabled-image", true)],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let root = temp_workspace("opensks-image-file");
        let mut runtime = ImageRuntime::new();
        let asset = runtime
            .generate_asset_file(
                &registry,
                &root,
                ImageAssetRequest {
                    id: "asset-file",
                    width: 32,
                    height: 16,
                    anchors: Vec::new(),
                    prompt: Some("durable image"),
                },
            )
            .expect("image asset file");
        let path = root.join(&asset.path);
        let bytes = fs::read(&path).expect("image bytes");
        assert!(bytes.starts_with(b"P6\n32 16\n255\n"));
        assert_eq!(asset.content_hash, hash_bytes(&bytes));
        assert!(
            asset
                .evidence_refs
                .contains(&"opensks-image:asset-file-verified".to_string())
        );
        assert!(
            runtime.ledger().provenance_receipts[0]
                .evidence_refs
                .contains(&"opensks-image:asset-file-verified".to_string())
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn provider_generated_image_file_uses_remote_model_and_hashes_returned_bytes() {
        let mut model = opensks_provider::fake_image_model("provider-1/image-model", true);
        model.provider_id = "provider-1".to_string();
        model.config_ref = opensks_contracts::SecretlessConfigRef {
            source: "provider-registry".to_string(),
            reference: "provider:provider-1:model:gpt-image-1.5".to_string(),
        };
        let registry = ModelRegistry::new(vec![model], PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() });
        let root = temp_workspace("opensks-provider-image-file");
        let client = ScriptedImageClient {
            seen: RefCell::new(Vec::new()),
            output: ImageProviderOutput {
                bytes: b"\x89PNG\r\n\x1a\nopensks-provider-image".to_vec(),
                extension: "png".to_string(),
                mime_type: Some("image/png".to_string()),
                evidence_refs: vec!["adapter:openai-compatible-images".to_string()],
            },
        };
        let mut runtime = ImageRuntime::new();
        let asset = runtime
            .generate_provider_asset_file(
                &registry,
                &root,
                ImageAssetRequest {
                    id: "provider-asset",
                    width: 64,
                    height: 64,
                    anchors: Vec::new(),
                    prompt: Some("render a provider image"),
                },
                &client,
            )
            .expect("provider image asset file");

        assert_eq!(
            client.seen.borrow().as_slice(),
            &[(
                "provider-1".to_string(),
                "provider-1/image-model".to_string(),
                "gpt-image-1.5".to_string(),
                "render a provider image".to_string(),
                64,
                64
            )]
        );
        assert_eq!(asset.path, ".opensks/assets/candidates/provider-asset.png");
        let bytes = fs::read(root.join(&asset.path)).expect("provider image bytes");
        assert_eq!(asset.content_hash, hash_bytes(&bytes));
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(
            asset
                .evidence_refs
                .contains(&"opensks-image:provider-asset-file-verified".to_string())
        );
        let receipt = runtime
            .ledger()
            .provenance_receipts
            .last()
            .expect("provider receipt");
        assert_eq!(receipt.asset_id, "provider-asset");
        assert_eq!(receipt.provider_id, "provider-1");
        assert_eq!(receipt.model_id, "provider-1/image-model");
        assert_eq!(receipt.content_hash, asset.content_hash);
        assert!(
            receipt
                .evidence_refs
                .contains(&"opensks-image:provider-asset-file-verified".to_string())
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn provider_inspection_reads_verified_asset_bytes_and_records_receipt() {
        let root = temp_workspace("provider-inspection");
        let registry = ModelRegistry::new(
            vec![
                opensks_provider::fake_image_model("image-generator", true),
                opensks_provider::fake_vision_model("vision-inspector", true),
            ],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let mut runtime = ImageRuntime::new();
        let asset = runtime
            .generate_asset_file(
                &registry,
                &root,
                ImageAssetRequest {
                    id: "inspect-asset",
                    width: 32,
                    height: 32,
                    anchors: Vec::new(),
                    prompt: Some("render an inspectable asset"),
                },
            )
            .expect("asset file");
        let client = ScriptedInspectionClient {
            seen: RefCell::new(Vec::new()),
            output: ImageInspectionProviderOutput {
                text: "A generated test pattern.".to_string(),
                evidence_refs: vec!["test:vision-client".to_string()],
            },
        };
        let result = runtime
            .inspect_provider_asset_file(
                &registry,
                &root,
                "artifact://.opensks/assets/candidates/inspect-asset.ppm",
                Some("describe the image"),
                &client,
            )
            .expect("inspection");

        assert_eq!(result.text, "A generated test pattern.");
        assert_eq!(result.receipt.asset_id, asset.id);
        assert_eq!(result.receipt.operation, ImageOperation::Inspect);
        assert_eq!(result.receipt.model_id, "vision-inspector");
        assert_eq!(result.receipt.content_hash, asset.content_hash);
        assert!(
            result
                .receipt
                .evidence_refs
                .contains(&"opensks-image:provider-image.inspect".to_string())
        );
        assert_eq!(
            client.seen.borrow().as_slice(),
            &[(
                "fake-local".to_string(),
                "vision-inspector".to_string(),
                "vision-inspector".to_string(),
                "inspect-asset".to_string(),
                asset.content_hash,
                "image/x-portable-pixmap".to_string(),
                "describe the image".to_string(),
            )]
        );
        assert_eq!(runtime.ledger().provenance_receipts.len(), 2);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn disabled_image_model_is_never_called() {
        let registry = ModelRegistry::new(
            vec![opensks_provider::fake_image_model("disabled-image", false)],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let mut runtime = ImageRuntime::new();
        let error = runtime
            .generate_asset(&registry, "asset-1", 512, 512, Vec::new(), None)
            .expect_err("missing model");
        assert!(matches!(error, ImageError::MissingImageModel));
    }

    #[test]
    fn unsafe_asset_id_is_rejected_before_path_write() {
        let registry = ModelRegistry::new(
            vec![opensks_provider::fake_image_model("enabled-image", true)],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let mut runtime = ImageRuntime::new();
        let error = runtime
            .generate_asset(&registry, "../escape", 32, 32, Vec::new(), None)
            .expect_err("unsafe id");
        assert!(matches!(error, ImageError::InvalidAssetId));
    }

    #[test]
    fn anchor_bounds_and_before_after_relation_are_checked() {
        let registry = ModelRegistry::new(
            vec![opensks_provider::fake_image_model("enabled-image", true)],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let mut runtime = ImageRuntime::new();
        let before = runtime
            .generate_asset(
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
                None,
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

    #[test]
    fn relate_before_after_rejects_unknown_before_id() {
        let registry = ModelRegistry::new(
            vec![opensks_provider::fake_image_model("enabled-image", true)],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let mut runtime = ImageRuntime::new();
        let before = runtime
            .generate_asset(&registry, "before", 100, 100, Vec::new(), None)
            .expect("before");
        let mut after = before.clone();
        after.id = "after".to_string();
        assert!(matches!(
            runtime.relate_before_after("does-not-exist", after),
            Err(ImageError::AssetNotFound)
        ));
    }

    #[test]
    fn image_inspection_uses_vision_capability_route() {
        let registry = ModelRegistry::new(
            vec![
                opensks_provider::fake_image_model("image-generator", true),
                opensks_provider::fake_vision_model("vision-inspector", true),
            ],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let mut runtime = ImageRuntime::new();
        let asset = runtime
            .generate_asset(&registry, "asset-1", 256, 256, Vec::new(), None)
            .expect("image asset");
        let receipt = runtime
            .inspect_asset(&registry, &asset.id, Some("describe regions"))
            .expect("inspection receipt");
        assert_eq!(receipt.asset_id, "asset-1");
        assert_eq!(receipt.model_id, "vision-inspector");
        assert_eq!(receipt.content_hash, asset.content_hash);
        assert!(receipt.route_receipt.requested_capabilities.vision_input);
        assert_eq!(runtime.ledger().provenance_receipts.len(), 2);
    }

    #[test]
    fn image_inspection_blocks_when_no_vision_model_is_enabled() {
        let image_registry = ModelRegistry::new(
            vec![opensks_provider::fake_image_model("image-generator", true)],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let no_vision_registry = ModelRegistry::new(
            vec![opensks_provider::fake_vision_model(
                "disabled-vision",
                false,
            )],
            PermissionPolicy { allow_provider_call_without_approval: true, ..PermissionPolicy::default() },
        );
        let mut runtime = ImageRuntime::new();
        let asset = runtime
            .generate_asset(&image_registry, "asset-1", 256, 256, Vec::new(), None)
            .expect("image asset");
        let error = runtime
            .inspect_asset(&no_vision_registry, &asset.id, None)
            .expect_err("missing vision route");
        assert!(matches!(error, ImageError::MissingImageModel));
    }

    fn temp_workspace(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).expect("temp workspace");
        root
    }

    type SeenImageRequest = (String, String, String, String, u32, u32);
    type SeenInspectionRequest = (String, String, String, String, String, String, String);

    struct ScriptedImageClient {
        seen: RefCell<Vec<SeenImageRequest>>,
        output: ImageProviderOutput,
    }

    impl ImageGenerationClient for ScriptedImageClient {
        fn generate_image(
            &self,
            request: &ImageProviderRequest<'_>,
        ) -> Result<ImageProviderOutput, ImageError> {
            self.seen.borrow_mut().push((
                request.provider_id.to_string(),
                request.model_id.to_string(),
                request.remote_model_id.to_string(),
                request.prompt.to_string(),
                request.width,
                request.height,
            ));
            Ok(self.output.clone())
        }
    }

    struct ScriptedInspectionClient {
        seen: RefCell<Vec<SeenInspectionRequest>>,
        output: ImageInspectionProviderOutput,
    }

    impl ImageInspectionClient for ScriptedInspectionClient {
        fn inspect_image(
            &self,
            request: &ImageInspectionProviderRequest<'_>,
        ) -> Result<ImageInspectionProviderOutput, ImageError> {
            self.seen.borrow_mut().push((
                request.provider_id.to_string(),
                request.model_id.to_string(),
                request.remote_model_id.to_string(),
                request.asset_id.to_string(),
                request.content_hash.to_string(),
                request.mime_type.to_string(),
                request.prompt.to_string(),
            ));
            assert!(!request.bytes.is_empty());
            Ok(self.output.clone())
        }
    }
}
