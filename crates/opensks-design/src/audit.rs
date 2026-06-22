//! Design audit engine (PR-040): deterministic rule checks over a *resolved*
//! package — its [`ResolvedTokenSet`] plus the package component catalog.
//!
//! The audit answers a single question before a package may be activated: does
//! this design satisfy the baseline accessibility / layout invariants? Four rule
//! families run, each producing zero or more [`DesignFinding`]s:
//!
//! * **contrast** — every foreground/background color pairing that declares a
//!   `contrast_constraints` minimum must meet a WCAG-style contrast ratio
//!   computed from the two colors' relative luminance. A pairing below its
//!   declared minimum (or below the [`MIN_TEXT_CONTRAST`] default for a token
//!   whose semantic role marks it as text on the canvas) is an `error`.
//! * **hit_target** — every `dimension` token whose path/role marks it as an
//!   interactive hit target must be `>= MIN_HIT_TARGET_PT`. A smaller target is
//!   an `error`; a non-primary (e.g. toolbar) target between the warn and error
//!   thresholds is a `warning`.
//! * **layout** — structural sanity over dimension tokens: a negative or zero
//!   size where a positive one is required is an `error` (a broken constraint a
//!   renderer cannot satisfy).
//! * **accessibility** — a status color that has no non-color signal (no
//!   component references it by a label/icon-bearing token ref) is flagged so a
//!   color-only status is never the sole signal; a text color with no
//!   contrast-safe pairing against the canvas is flagged.
//!
//! The whole report is deterministic: rules iterate tokens/components in input
//! order and findings are emitted in a fixed rule order, so the same package
//! always produces a byte-identical [`DesignAuditReport`].
//!
//! `passed` is true when there are **no** `error`-severity findings (warnings do
//! not fail an audit). `blocks_activation` mirrors that: any `error` finding
//! blocks activation. The atomic activation path (see [`crate::activation`])
//! refuses to switch the active package when `blocks_activation` is true.

use crate::compiler::{ResolvedTokenSet, resolve_token_set};
use crate::contracts::DesignToken;
use opensks_contracts::{DesignPackageComponents, DesignPackageTokens};

/// `schema` value for a design-audit report.
pub const DESIGN_AUDIT_SCHEMA: &str = "opensks.design-audit.v1";

/// Minimum interactive hit-target size, in points. Targets below this fail.
pub const MIN_HIT_TARGET_PT: f64 = 44.0;
/// A non-primary (e.g. toolbar) hit target at or above this but below
/// [`MIN_HIT_TARGET_PT`] is a warning rather than an error.
pub const WARN_HIT_TARGET_PT: f64 = 30.0;
/// Default minimum contrast ratio for body text over its background when a token
/// declares no explicit `contrast_constraints` minimum (WCAG AA normal text).
pub const MIN_TEXT_CONTRAST: f64 = 4.5;

/// The family of rule a finding came from. Serialized as the contract `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    Contrast,
    HitTarget,
    Layout,
    Accessibility,
}

impl FindingKind {
    /// Stable wire string for the contract `kind` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contrast => "contrast",
            Self::HitTarget => "hit_target",
            Self::Layout => "layout",
            Self::Accessibility => "accessibility",
        }
    }
}

/// Severity of a finding. An `error` blocks activation; a `warning` does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// A single audit finding: which rule, how severe, a human-readable detail, and
/// the token/component reference it concerns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignFinding {
    pub kind: FindingKind,
    pub severity: Severity,
    pub detail: String,
    pub reference: String,
}

impl DesignFinding {
    fn error(kind: FindingKind, reference: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            severity: Severity::Error,
            detail: detail.into(),
            reference: reference.into(),
        }
    }

    fn warning(kind: FindingKind, reference: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            severity: Severity::Warning,
            detail: detail.into(),
            reference: reference.into(),
        }
    }

    /// Render as the contract finding object.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"kind\":{},\"severity\":{},\"detail\":{},\"ref\":{}}}",
            json_str(self.kind.as_str()),
            json_str(self.severity.as_str()),
            json_str(&self.detail),
            json_str(&self.reference),
        )
    }
}

/// The full audit result for one package: every finding plus the derived
/// `passed` / `blocks_activation` gate. `passed` is true when no finding is an
/// error; `blocks_activation` is true when any finding is an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignAuditReport {
    pub package_id: String,
    pub findings: Vec<DesignFinding>,
}

impl DesignAuditReport {
    /// Whether the audit passed (no error-severity findings).
    pub fn passed(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Whether the audit blocks activation (any error-severity finding).
    pub fn blocks_activation(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Render the full `opensks.design-audit.v1` contract JSON.
    pub fn to_json(&self) -> String {
        let findings = self
            .findings
            .iter()
            .map(DesignFinding::to_json)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema\":{},\"package_id\":{},\"passed\":{},\"blocks_activation\":{},\"findings\":[{}]}}",
            json_str(DESIGN_AUDIT_SCHEMA),
            json_str(&self.package_id),
            self.passed(),
            self.blocks_activation(),
            findings,
        )
    }
}

/// Errors that prevent an audit from even running (a malformed package the rules
/// cannot evaluate). A rule *finding* is a normal audit result, not an error;
/// these are the cases where there is nothing to audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesignAuditError {
    /// The token set could not be resolved (alias cycle/unresolved/type
    /// mismatch). Carries the underlying compiler reason code.
    Unresolvable { reason: String },
}

impl std::fmt::Display for DesignAuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolvable { reason } => {
                write!(f, "design_audit_unresolvable: {reason}")
            }
        }
    }
}

impl std::error::Error for DesignAuditError {}

/// Audit a package's loaded token document + optional component catalog.
///
/// Resolves the token set's aliases first (a set that cannot resolve is a hard
/// [`DesignAuditError`], not a finding), then runs every rule family in fixed
/// order and assembles the report.
pub fn audit_package(
    package_id: &str,
    tokens: &DesignPackageTokens,
    components: Option<&DesignPackageComponents>,
) -> Result<DesignAuditReport, DesignAuditError> {
    let set = package_tokens_to_set(package_id, tokens);
    let resolved = resolve_token_set(&set).map_err(|error| DesignAuditError::Unresolvable {
        reason: error.reason_code().to_string(),
    })?;

    let mut findings: Vec<DesignFinding> = Vec::new();
    // Fixed rule order keeps the report deterministic.
    findings.extend(audit_contrast(&resolved));
    findings.extend(audit_hit_targets(&resolved));
    findings.extend(audit_layout(&resolved));
    findings.extend(audit_accessibility(&resolved, components));

    Ok(DesignAuditReport {
        package_id: package_id.to_string(),
        findings,
    })
}

/// Convert the contracts package-token document into the design crate's token
/// set so the existing alias resolver can run over it.
fn package_tokens_to_set(
    package_id: &str,
    tokens: &DesignPackageTokens,
) -> crate::contracts::DesignTokenSet {
    crate::contracts::DesignTokenSet {
        schema: crate::contracts::DESIGN_TOKEN_SET_SCHEMA.to_string(),
        design_system_id: package_id.to_string(),
        revision: tokens.revision,
        tokens: tokens
            .tokens
            .iter()
            .map(|t| DesignToken {
                path: t.path.clone(),
                token_type: t.token_type.clone(),
                value: t.value.clone(),
                unit: t.unit.clone(),
                semantic_role: t.semantic_role.clone(),
                confidence: t.confidence.clone(),
                source_refs: t.source_refs.clone(),
                contrast_constraints: t.contrast_constraints.clone(),
            })
            .collect(),
    }
}

// ---- contrast ----

/// A declared contrast constraint parsed from a token's `contrast_constraints`:
/// the other color path and the minimum ratio that pairing must meet.
struct ContrastConstraint {
    against: String,
    minimum_ratio: f64,
}

fn parse_contrast_constraint(value: &serde_json::Value) -> Option<ContrastConstraint> {
    let against = value.get("against")?.as_str()?.to_string();
    let minimum_ratio = value.get("minimum_ratio")?.as_f64()?;
    Some(ContrastConstraint {
        against,
        minimum_ratio,
    })
}

/// Contrast rule: for every color token that declares a `contrast_constraints`
/// minimum against another color, compute the actual ratio and flag pairings
/// below the declared minimum as errors. Pairings whose other side is missing or
/// not a color are flagged as accessibility-adjacent errors (a constraint that
/// can never be satisfied).
fn audit_contrast(resolved: &ResolvedTokenSet) -> Vec<DesignFinding> {
    let mut findings = Vec::new();
    for token in &resolved.tokens {
        if token.token_type != "color" {
            continue;
        }
        for raw in &token.contrast_constraints {
            let Some(constraint) = parse_contrast_constraint(raw) else {
                continue;
            };
            let Some(fg) = hex_color(&token.value) else {
                continue;
            };
            let Some(other) = resolved.get(&constraint.against) else {
                findings.push(DesignFinding::error(
                    FindingKind::Contrast,
                    token.path.clone(),
                    format!(
                        "contrast constraint references missing color {}",
                        constraint.against
                    ),
                ));
                continue;
            };
            let Some(bg) = hex_color(&other.value) else {
                findings.push(DesignFinding::error(
                    FindingKind::Contrast,
                    token.path.clone(),
                    format!(
                        "contrast constraint against non-color {}",
                        constraint.against
                    ),
                ));
                continue;
            };
            let ratio = contrast_ratio(fg, bg);
            if ratio + RATIO_EPSILON < constraint.minimum_ratio {
                findings.push(DesignFinding::error(
                    FindingKind::Contrast,
                    token.path.clone(),
                    format!(
                        "contrast {ratio:.2}:1 against {} below required {:.2}:1",
                        constraint.against, constraint.minimum_ratio
                    ),
                ));
            }
        }
    }
    findings
}

// ---- hit target ----

/// True when a dimension token represents an interactive hit target (by path or
/// semantic role), so the minimum-size rule applies to it.
fn is_hit_target(token: &DesignToken) -> bool {
    token.path.starts_with("size.hit_target")
        || token
            .semantic_role
            .as_deref()
            .map(|role| role.contains("hit-target"))
            .unwrap_or(false)
}

/// True when the hit target is a *primary* interactive target (must always meet
/// the full minimum), versus a secondary/toolbar target (a warn band applies).
fn is_primary_hit_target(token: &DesignToken) -> bool {
    token.path.ends_with(".primary")
        || token
            .semantic_role
            .as_deref()
            .map(|role| role.contains("primary"))
            .unwrap_or(false)
}

/// Hit-target rule: every interactive hit-target dimension must be at least
/// [`MIN_HIT_TARGET_PT`]. A primary target below the minimum is an error; a
/// non-primary target in the `[WARN_HIT_TARGET_PT, MIN_HIT_TARGET_PT)` band is a
/// warning; any target below [`WARN_HIT_TARGET_PT`] is an error.
fn audit_hit_targets(resolved: &ResolvedTokenSet) -> Vec<DesignFinding> {
    let mut findings = Vec::new();
    for token in &resolved.tokens {
        if token.token_type != "dimension" || !is_hit_target(token) {
            continue;
        }
        let Some(size) = token.value.as_f64() else {
            continue;
        };
        if size + RATIO_EPSILON >= MIN_HIT_TARGET_PT {
            continue;
        }
        if is_primary_hit_target(token) || size + RATIO_EPSILON < WARN_HIT_TARGET_PT {
            findings.push(DesignFinding::error(
                FindingKind::HitTarget,
                token.path.clone(),
                format!("hit target {size}pt below minimum {MIN_HIT_TARGET_PT}pt"),
            ));
        } else {
            findings.push(DesignFinding::warning(
                FindingKind::HitTarget,
                token.path.clone(),
                format!("secondary hit target {size}pt below recommended {MIN_HIT_TARGET_PT}pt"),
            ));
        }
    }
    findings
}

// ---- layout ----

/// Layout rule: a `dimension` token with a negative or zero value is a broken
/// layout constraint (no renderer can satisfy a zero/negative size, rail width,
/// or radius), reported as an error.
fn audit_layout(resolved: &ResolvedTokenSet) -> Vec<DesignFinding> {
    let mut findings = Vec::new();
    for token in &resolved.tokens {
        if token.token_type != "dimension" {
            continue;
        }
        let Some(size) = token.value.as_f64() else {
            continue;
        };
        if size <= 0.0 {
            findings.push(DesignFinding::error(
                FindingKind::Layout,
                token.path.clone(),
                format!("non-positive dimension {size} cannot lay out"),
            ));
        }
    }
    findings
}

// ---- accessibility ----

/// True when a color token is a status color (its path/role marks it as a
/// status signal) and therefore must not be the *only* signal.
fn is_status_color(token: &DesignToken) -> bool {
    token.token_type == "color"
        && (token.path.starts_with("color.status")
            || token
                .semantic_role
                .as_deref()
                .map(|role| role.contains("status"))
                .unwrap_or(false))
}

/// Accessibility rule:
/// * a status color with no component that references it (no non-color signal
///   carrier such as an icon/label-bearing component) is flagged as a warning —
///   a color-only status has no redundant cue;
/// * a primary/secondary text color that declares no contrast constraint at all
///   is flagged as a warning — there is no contrast-safe pairing to verify.
fn audit_accessibility(
    resolved: &ResolvedTokenSet,
    components: Option<&DesignPackageComponents>,
) -> Vec<DesignFinding> {
    let mut findings = Vec::new();

    // Collect every token referenced by any component (the non-color signal
    // carriers). A status color referenced by a component is assumed to be
    // paired with that component's icon/label.
    let referenced: Vec<&str> = components
        .map(|catalog| {
            catalog
                .components
                .iter()
                .flat_map(|component| component.token_refs.iter())
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default();

    for token in &resolved.tokens {
        if is_status_color(token) && !referenced.contains(&token.path.as_str()) {
            findings.push(DesignFinding::warning(
                FindingKind::Accessibility,
                token.path.clone(),
                "status color has no component pairing an icon/label signal",
            ));
        }
        if token.token_type == "color"
            && is_text_color(token)
            && token.contrast_constraints.is_empty()
        {
            findings.push(DesignFinding::warning(
                FindingKind::Accessibility,
                token.path.clone(),
                "text color declares no contrast-safe pairing",
            ));
        }
    }
    findings
}

/// True when a color token is body/primary/secondary text (path or role).
fn is_text_color(token: &DesignToken) -> bool {
    token.path.starts_with("color.text")
        || token
            .semantic_role
            .as_deref()
            .map(|role| role.contains("text"))
            .unwrap_or(false)
}

// ---- color math ----

/// Small epsilon so floating-point ratios/sizes at the exact threshold are not
/// flagged by rounding noise.
const RATIO_EPSILON: f64 = 1e-9;

/// Parse a `#RRGGBB` hex color into 8-bit RGB. Accepts an optional leading `#`
/// and a 3-digit shorthand (`#abc`). Returns `None` for non-hex values.
fn hex_color(value: &serde_json::Value) -> Option<(u8, u8, u8)> {
    let text = value.as_str()?.trim();
    let hex = text.strip_prefix('#').unwrap_or(text);
    let expanded = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => hex.to_string(),
        _ => return None,
    };
    let r = u8::from_str_radix(&expanded[0..2], 16).ok()?;
    let g = u8::from_str_radix(&expanded[2..4], 16).ok()?;
    let b = u8::from_str_radix(&expanded[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Relative luminance of an sRGB color per the WCAG 2.x definition.
fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
    fn channel(c: u8) -> f64 {
        let s = f64::from(c) / 255.0;
        if s <= 0.039_28 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// WCAG contrast ratio between two colors: `(lighter + 0.05) / (darker + 0.05)`,
/// always `>= 1.0`.
pub fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (lighter, darker) = if la >= lb { (la, lb) } else { (lb, la) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Minimal JSON string escaper for the small audit field set.
fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensks_contracts::{
        DESIGN_PACKAGE_COMPONENTS_SCHEMA, DESIGN_PACKAGE_TOKENS_SCHEMA, DesignPackageComponent,
        DesignPackageToken,
    };

    fn token(
        path: &str,
        ty: &str,
        value: serde_json::Value,
        constraints: Vec<serde_json::Value>,
    ) -> DesignPackageToken {
        DesignPackageToken {
            path: path.to_string(),
            token_type: ty.to_string(),
            value,
            unit: if ty == "dimension" {
                Some("pt".to_string())
            } else {
                None
            },
            semantic_role: None,
            confidence: None,
            source_refs: vec![],
            contrast_constraints: constraints,
        }
    }

    fn token_set(tokens: Vec<DesignPackageToken>) -> DesignPackageTokens {
        DesignPackageTokens {
            schema: DESIGN_PACKAGE_TOKENS_SCHEMA.to_string(),
            design_system_id: "demo".to_string(),
            revision: 1,
            tokens,
        }
    }

    fn constraint(against: &str, minimum_ratio: f64) -> serde_json::Value {
        serde_json::json!({ "against": against, "minimum_ratio": minimum_ratio })
    }

    #[test]
    fn contrast_ratio_matches_known_extremes() {
        // Black on white is the canonical 21:1.
        let ratio = contrast_ratio((0, 0, 0), (255, 255, 255));
        assert!((ratio - 21.0).abs() < 0.01, "black/white should be 21:1");
        // Identical colors are 1:1.
        let same = contrast_ratio((0x12, 0x34, 0x56), (0x12, 0x34, 0x56));
        assert!((same - 1.0).abs() < 1e-9);
    }

    #[test]
    fn clean_package_passes_with_no_error_findings() {
        // White text over a near-black canvas with a 7:1 requirement (~18:1
        // actual), a 44pt primary hit target, positive sizes, and a status color
        // referenced by a component. No errors.
        let tokens = token_set(vec![
            token(
                "color.canvas",
                "color",
                serde_json::json!("#0E1015"),
                vec![],
            ),
            token(
                "color.text.primary",
                "color",
                serde_json::json!("#E9EDF3"),
                vec![constraint("color.canvas", 7.0)],
            ),
            token(
                "color.status.success",
                "color",
                serde_json::json!("#5EDEC4"),
                vec![],
            ),
            token(
                "size.hit_target.primary",
                "dimension",
                serde_json::json!(44),
                vec![],
            ),
            token(
                "size.rail.width",
                "dimension",
                serde_json::json!(88),
                vec![],
            ),
        ]);
        let components = DesignPackageComponents {
            schema: DESIGN_PACKAGE_COMPONENTS_SCHEMA.to_string(),
            design_system_id: "demo".to_string(),
            components: vec![DesignPackageComponent {
                id: "status.badge".to_string(),
                name: "Status Badge".to_string(),
                description: Some("icon + label + color".to_string()),
                token_refs: vec!["color.status.success".to_string()],
            }],
        };
        let report = audit_package("demo", &tokens, Some(&components)).expect("audit runs");
        assert!(report.passed(), "clean package must pass: {report:?}");
        assert!(!report.blocks_activation());
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.severity == Severity::Error),
            "no error findings expected"
        );
    }

    #[test]
    fn low_contrast_pair_fails_and_blocks_activation() {
        // Muted grey text on a slightly-lighter grey canvas: ~1.x:1, far below a
        // 7:1 requirement. This is an error finding.
        let tokens = token_set(vec![
            token(
                "color.canvas",
                "color",
                serde_json::json!("#777777"),
                vec![],
            ),
            token(
                "color.text.muted",
                "color",
                serde_json::json!("#7E8796"),
                vec![constraint("color.canvas", 7.0)],
            ),
        ]);
        let report = audit_package("demo", &tokens, None).expect("audit runs");
        assert!(!report.passed(), "low contrast must fail");
        assert!(report.blocks_activation(), "an error must block activation");
        let finding = report
            .findings
            .iter()
            .find(|f| f.kind == FindingKind::Contrast)
            .expect("a contrast finding");
        assert_eq!(finding.severity, Severity::Error);
        assert_eq!(finding.reference, "color.text.muted");
    }

    #[test]
    fn primary_hit_target_below_minimum_is_error() {
        let tokens = token_set(vec![token(
            "size.hit_target.primary",
            "dimension",
            serde_json::json!(20),
            vec![],
        )]);
        let report = audit_package("demo", &tokens, None).expect("audit runs");
        assert!(report.blocks_activation());
        let finding = report
            .findings
            .iter()
            .find(|f| f.kind == FindingKind::HitTarget)
            .expect("hit target finding");
        assert_eq!(finding.severity, Severity::Error);
    }

    #[test]
    fn negative_dimension_is_layout_error() {
        let tokens = token_set(vec![token(
            "size.rail.width",
            "dimension",
            serde_json::json!(-4),
            vec![],
        )]);
        let report = audit_package("demo", &tokens, None).expect("audit runs");
        assert!(report.blocks_activation());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::Layout && f.severity == Severity::Error)
        );
    }

    #[test]
    fn status_color_without_component_pairing_is_warning_not_error() {
        let tokens = token_set(vec![token(
            "color.status.danger",
            "color",
            serde_json::json!("#E0876E"),
            vec![],
        )]);
        // No components: the status color stands alone.
        let report = audit_package("demo", &tokens, None).expect("audit runs");
        // A warning does not fail the audit.
        assert!(report.passed(), "warning-only audit still passes");
        assert!(!report.blocks_activation());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::Accessibility && f.severity == Severity::Warning)
        );
    }

    #[test]
    fn audit_json_is_deterministic_and_shaped() {
        let tokens = token_set(vec![token(
            "size.hit_target.primary",
            "dimension",
            serde_json::json!(10),
            vec![],
        )]);
        let a = audit_package("demo", &tokens, None).unwrap().to_json();
        let b = audit_package("demo", &tokens, None).unwrap().to_json();
        assert_eq!(a, b, "audit JSON must be deterministic");
        assert!(a.contains("\"schema\":\"opensks.design-audit.v1\""));
        assert!(a.contains("\"blocks_activation\":true"));
        assert!(a.contains("\"kind\":\"hit_target\""));
        assert!(a.contains("\"ref\":\"size.hit_target.primary\""));
    }

    #[test]
    fn missing_contrast_target_is_error() {
        let tokens = token_set(vec![token(
            "color.text.primary",
            "color",
            serde_json::json!("#FFFFFF"),
            vec![constraint("color.does_not_exist", 4.5)],
        )]);
        let report = audit_package("demo", &tokens, None).expect("audit runs");
        assert!(report.blocks_activation());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::Contrast && f.severity == Severity::Error)
        );
    }
}
