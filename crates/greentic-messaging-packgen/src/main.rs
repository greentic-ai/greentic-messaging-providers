use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use greentic_types::PROVIDER_EXTENSION_ID;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(name = "greentic-messaging-packgen")]
#[command(about = "Deterministic generator for messaging provider packs")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Generate {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        write_default_spec: Option<PathBuf>,
    },
    GenerateAll {
        #[arg(long)]
        spec_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct Spec {
    pack: PackSpec,
    provider: ProviderSpec,
    components: ComponentsSpec,
    flows: FlowsSpec,
    setup: Option<SetupSpec>,
    requirements: Option<RequirementsSpec>,
    validators: Option<Vec<ValidatorSpec>>,
    source: Option<SourceSpec>,
    contract: Option<ContractSpec>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PackSpec {
    id: String,
    name: Option<String>,
    #[serde(default = "default_version")]
    version: String,
    publisher: Option<String>,
    kind: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProviderSpec {
    provider_type: String,
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default = "default_ops")]
    ops: Vec<String>,
    #[serde(default = "default_capabilities")]
    capabilities: serde_json::Value,
    #[serde(default)]
    config_schema_ref: Option<String>,
    #[serde(default)]
    state_schema_ref: Option<String>,
    #[serde(default)]
    docs_ref: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ComponentsSpec {
    adapter: ComponentSpec,
    renderer: Option<ComponentSpec>,
    ingress: Option<ComponentSpec>,
    subscriptions: Option<ComponentSpec>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ComponentSpec {
    component_ref: String,
    world: String,
    export: Option<String>,
    id: Option<String>,
    manifest: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
struct FlowsSpec {
    #[serde(default)]
    generate: Vec<String>,
    #[serde(default)]
    include: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SetupSpec {
    #[serde(default)]
    questions: Vec<SetupQuestionSpec>,
    #[serde(default)]
    emits_success_message: Option<bool>,
    asset_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SetupQuestionSpec {
    key: String,
    prompt: String,
    kind: String,
    required: Option<bool>,
    help: Option<String>,
    choices: Option<Vec<String>>,
    default: Option<serde_json::Value>,
    validate: Option<serde_json::Value>,
    write_to: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct RequirementsSpec {
    #[serde(default)]
    required_args: Vec<String>,
    #[serde(default)]
    optional_args: Vec<String>,
    #[serde(default)]
    examples: Vec<RequirementsExample>,
    notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RequirementsExample {
    args: serde_json::Value,
    text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ValidatorSpec {
    id: String,
    component_ref: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct SourceSpec {
    pack_dir: PathBuf,
    extensions: SourceExtensionsSpec,
    copy: Option<SourceCopySpec>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SourceExtensionsSpec {
    #[serde(default)]
    include: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SourceCopySpec {
    components: Option<bool>,
    assets: Option<bool>,
    fixtures: Option<bool>,
    schemas: Option<bool>,
    root_files: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ContractSpec {
    ingress: Option<IngressSpec>,
    webhooks: Option<WebhookSpec>,
    subscriptions: Option<SubscriptionsSpec>,
    render: Option<RenderSpec>,
}

#[derive(Debug, Deserialize, Serialize)]
struct IngressSpec {
    mode: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct WebhookSpec {
    required: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct SubscriptionsSpec {
    required: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct RenderSpec {
    required: bool,
}

fn default_version() -> String {
    "0.0.0-dev".to_string()
}

fn default_ops() -> Vec<String> {
    vec!["send".to_string()]
}

fn default_capabilities() -> serde_json::Value {
    json!({ "render_tiers": ["tier-d"] })
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Generate {
            spec,
            out,
            write_default_spec,
        } => generate(&spec, &out, write_default_spec.as_deref()),
        Commands::GenerateAll { spec_dir, out } => generate_all(&spec_dir, &out),
    }
}

fn generate(spec_path: &Path, out_dir: &Path, write_default_spec: Option<&Path>) -> Result<()> {
    if let Some(path) = write_default_spec {
        write_default_spec_file(path)?;
    }

    let spec = load_spec(spec_path)?;
    validate_spec(spec_path, &spec)?;
    let flows = flows_list(&spec)?;

    let out_dir = absolute_path(out_dir)?;
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir)
            .with_context(|| format!("cleaning output dir {}", out_dir.display()))?;
    }
    pack_new(&out_dir, &spec.pack.id)?;

    let flows_dir = out_dir.join("flows");
    let components_dir = out_dir.join("components");
    let assets_dir = out_dir.join("assets");

    if let Some(source) = &spec.source {
        generate_from_source(
            spec_path,
            &out_dir,
            source,
            &flows,
            &flows_dir,
            &components_dir,
            &assets_dir,
            &spec,
        )?;
    } else {
        let schema_root = out_dir.join("schemas").join("messaging");
        let schema_dir = schema_root.join(provider_id(&spec));
        fs::create_dir_all(&assets_dir)?;
        fs::create_dir_all(&schema_dir)?;

        write_setup_yaml(&assets_dir.join("setup.yaml"), &spec)?;
        write_setup_yaml(&out_dir.join("setup.yaml"), &spec)?;
        write_empty_secret_requirements(&out_dir)?;
        stage_components(&components_dir, spec_path, &spec, &flows)?;
        stage_schema(&schema_dir, &schema_root, spec_path, &spec)?;

        let answers_dir = out_dir.join(".packgen-answers");
        fs::create_dir_all(&answers_dir)?;
        remove_default_flow(&flows_dir)?;
        generate_flows(&flows_dir, &answers_dir, spec_path, &spec, &flows)?;
        fs::remove_dir_all(&answers_dir).ok();

        for flow_name in &flows {
            run_flow_doctor(&flows_dir.join(format!("{flow_name}.ygtc")))?;
        }

        pack_update(&out_dir)?;
        update_pack_yaml(&out_dir, &spec, None, false)?;
        add_provider_extension(
            &out_dir,
            &spec.provider.provider_type,
            "messaging",
            spec.validators.as_ref().and_then(|v| v.first()),
        )?;
        update_pack_manifest(&out_dir)?;
        verify_pack_dir(&out_dir, &spec, &flows)?;
    }

    Ok(())
}

fn generate_all(spec_dir: &Path, out_dir: &Path) -> Result<()> {
    let mut specs = Vec::new();
    for entry in fs::read_dir(spec_dir)
        .with_context(|| format!("reading spec dir {}", spec_dir.display()))?
    {
        let path = entry?.path();
        let Some(ext) = path.extension().and_then(|v| v.to_str()) else {
            continue;
        };
        if !matches!(ext, "yaml" | "yml" | "json") {
            continue;
        }
        specs.push(path);
    }
    specs.sort();
    fs::create_dir_all(out_dir)?;

    for spec_path in specs {
        let spec = load_spec(&spec_path)?;
        validate_spec(&spec_path, &spec)?;
        let pack_out = out_dir.join(&spec.pack.id);
        generate(&spec_path, &pack_out, None)?;
    }

    Ok(())
}

fn flows_list(spec: &Spec) -> Result<Vec<String>> {
    if !spec.flows.include.is_empty() {
        return Ok(spec.flows.include.clone());
    }
    if !spec.flows.generate.is_empty() {
        return Ok(spec.flows.generate.clone());
    }
    Err(anyhow::anyhow!(
        "flows.generate or flows.include must be provided"
    ))
}

fn load_spec(path: &Path) -> Result<Spec> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("reading spec {}", path.display()))?;
    let spec: Spec = serde_yaml::from_str(&contents)
        .with_context(|| format!("parsing spec {}", path.display()))?;
    Ok(spec)
}

fn allowed_flow_names() -> BTreeSet<&'static str> {
    // TODO: source from greentic-interfaces/types once canonical entry flow names are exposed.
    [
        "setup_default",
        "setup_custom",
        "diagnostics",
        "requirements",
        "verify_webhooks",
        "sync_subscriptions",
        "rotate_credentials",
    ]
    .into_iter()
    .collect()
}

fn allowed_provider_ops() -> BTreeSet<&'static str> {
    // TODO: source from greentic-types/greentic-interfaces once canonical ops are exposed.
    [
        "send",
        "reply",
        "ingest",
        "subscription_ensure",
        "subscription_renew",
        "subscription_delete",
    ]
    .into_iter()
    .collect()
}

fn validate_spec(spec_path: &Path, spec: &Spec) -> Result<()> {
    if spec.pack.id.trim().is_empty() {
        return Err(anyhow::anyhow!("spec.pack.id must be set"));
    }
    if spec.provider.provider_type.trim().is_empty() {
        return Err(anyhow::anyhow!("spec.provider.provider_type must be set"));
    }
    let flows = flows_list(spec)?;
    let allowed_ops = allowed_provider_ops();
    let mut seen_ops = BTreeSet::new();
    for op in &spec.provider.ops {
        if op.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "spec.provider.ops must not include empty op names"
            ));
        }
        if !allowed_ops.contains(op.as_str()) {
            return Err(anyhow::anyhow!(
                "spec.provider.ops contains unknown op '{op}'. Allowed ops: {allowed_ops:?}"
            ));
        }
        if !seen_ops.insert(op) {
            return Err(anyhow::anyhow!(
                "spec.provider.ops contains duplicate op '{op}'"
            ));
        }
    }
    if spec.provider.ops.is_empty() {
        return Err(anyhow::anyhow!(
            "spec.provider.ops must include at least one op"
        ));
    }
    let allowed_flows = allowed_flow_names();
    let flow_set: BTreeSet<_> = flows.iter().map(|f| f.as_str()).collect();
    for flow in &flows {
        if !allowed_flows.contains(flow.as_str()) {
            return Err(anyhow::anyhow!(
                "flows include contains unknown flow '{flow}'. Allowed flows: {allowed_flows:?}"
            ));
        }
    }
    if !flow_set.contains("setup_default") {
        return Err(anyhow::anyhow!(
            "flows must include setup_default (via flows.include or flows.generate)"
        ));
    }
    if !flow_set.contains("requirements") {
        return Err(anyhow::anyhow!(
            "flows must include requirements (via flows.include or flows.generate)"
        ));
    }
    if spec.source.is_none() {
        if spec.setup.is_none() {
            return Err(anyhow::anyhow!(
                "setup section must be present when generating setup_default"
            ));
        }
        if spec.requirements.is_none() {
            return Err(anyhow::anyhow!(
                "requirements section must be present when generating requirements flow"
            ));
        }
    }
    if spec.components.adapter.export.is_none() {
        return Err(anyhow::anyhow!("components.adapter.export must be set"));
    }
    if !spec.components.adapter.component_ref.starts_with("local:")
        && !spec.components.adapter.component_ref.starts_with("oci://")
    {
        return Err(anyhow::anyhow!(
            "components.adapter.component_ref must use local: or oci://"
        ));
    }
    if spec.components.adapter.world.trim().is_empty() {
        return Err(anyhow::anyhow!("components.adapter.world must be set"));
    }
    if spec.source.is_some() && spec.components.adapter.id.is_none() {
        return Err(anyhow::anyhow!(
            "components.adapter.id must be set when source.pack_dir is used"
        ));
    }
    let known_worlds = known_worlds_from_interfaces()?;
    if !known_worlds.contains(&spec.components.adapter.world) {
        return Err(anyhow::anyhow!(
            "components.adapter.world '{}' is not a known WIT world from greentic-interfaces. Run packgen --write-default-spec to see canonical values.",
            spec.components.adapter.world
        ));
    }
    if let Some(renderer) = &spec.components.renderer {
        if renderer.world.trim().is_empty() {
            return Err(anyhow::anyhow!("components.renderer.world must be set"));
        }
        if !known_worlds.contains(&renderer.world) {
            return Err(anyhow::anyhow!(
                "components.renderer.world '{}' is not a known WIT world from greentic-interfaces.",
                renderer.world
            ));
        }
    }
    if let Some(ingress) = &spec.components.ingress {
        if ingress.world.trim().is_empty() {
            return Err(anyhow::anyhow!("components.ingress.world must be set"));
        }
        if ingress.export.is_none() {
            return Err(anyhow::anyhow!("components.ingress.export must be set"));
        }
        if spec.source.is_none() && !known_worlds.contains(&ingress.world) {
            return Err(anyhow::anyhow!(
                "components.ingress.world '{}' is not a known WIT world from greentic-interfaces.",
                ingress.world
            ));
        }
        if !ingress.component_ref.starts_with("local:")
            && !ingress.component_ref.starts_with("oci://")
        {
            return Err(anyhow::anyhow!(
                "components.ingress.component_ref must use local: or oci://"
            ));
        }
    }
    if let Some(subscriptions) = &spec.components.subscriptions {
        if subscriptions.world.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "components.subscriptions.world must be set"
            ));
        }
        if subscriptions.export.is_none() {
            return Err(anyhow::anyhow!(
                "components.subscriptions.export must be set"
            ));
        }
        if spec.source.is_none() && !known_worlds.contains(&subscriptions.world) {
            return Err(anyhow::anyhow!(
                "components.subscriptions.world '{}' is not a known WIT world from greentic-interfaces.",
                subscriptions.world
            ));
        }
        if !subscriptions.component_ref.starts_with("local:")
            && !subscriptions.component_ref.starts_with("oci://")
        {
            return Err(anyhow::anyhow!(
                "components.subscriptions.component_ref must use local: or oci://"
            ));
        }
    }
    validate_render_tiers(spec)?;
    if let Some(setup) = &spec.setup {
        for question in &setup.questions {
            if !question.write_to.starts_with("config:")
                && !question.write_to.starts_with("secrets:")
            {
                return Err(anyhow::anyhow!(
                    "setup.questions[].write_to must start with config: or secrets:"
                ));
            }
        }
    }
    if let Some(source) = &spec.source {
        validate_source_spec(spec_path, spec, source, &flows)?;
    }
    if let Some(contract) = &spec.contract {
        validate_contract(spec, contract, &flows)?;
    }
    Ok(())
}

fn validate_render_tiers(spec: &Spec) -> Result<()> {
    let Some(render_tiers) = spec.provider.capabilities.get("render_tiers") else {
        return Ok(());
    };
    let Some(values) = render_tiers.as_array() else {
        return Err(anyhow::anyhow!(
            "provider.capabilities.render_tiers must be an array of strings"
        ));
    };
    let allowed = known_render_tiers_from_interfaces()?;
    for value in values {
        let Some(tier) = value.as_str() else {
            return Err(anyhow::anyhow!(
                "provider.capabilities.render_tiers entries must be strings"
            ));
        };
        if !allowed.contains(tier) {
            return Err(anyhow::anyhow!(
                "provider.capabilities.render_tiers contains unknown value '{tier}'. Allowed values: {allowed:?}"
            ));
        }
    }
    Ok(())
}

fn validate_source_spec(
    spec_path: &Path,
    spec: &Spec,
    source: &SourceSpec,
    flows: &[String],
) -> Result<()> {
    let spec_dir = spec_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("spec has no parent directory"))?;
    let source_pack_dir = resolve_local_path(spec_dir, &source.pack_dir)?;
    if !source_pack_dir.exists() {
        return Err(anyhow::anyhow!(
            "source.pack_dir does not exist: {}",
            source_pack_dir.display()
        ));
    }
    if spec.flows.include.is_empty() {
        return Err(anyhow::anyhow!(
            "flows.include must be set when source.pack_dir is used"
        ));
    }
    let source_pack_yaml = source_pack_dir.join("pack.yaml");
    if !source_pack_yaml.exists() {
        return Err(anyhow::anyhow!(
            "source pack.yaml not found at {}",
            source_pack_yaml.display()
        ));
    }
    let source_flow_dir = source_pack_dir.join("flows");
    if !source_flow_dir.exists() {
        return Err(anyhow::anyhow!(
            "source flows directory not found at {}",
            source_flow_dir.display()
        ));
    }
    for flow in flows {
        let has_flow = flow_files_for(&source_flow_dir, flow)?
            .into_iter()
            .any(|path| path.extension().and_then(|v| v.to_str()) == Some("ygtc"));
        if !has_flow {
            return Err(anyhow::anyhow!(
                "source pack missing flow '{}' under {}",
                flow,
                source_flow_dir.display()
            ));
        }
    }

    let source_pack = load_pack_yaml(&source_pack_yaml)?;
    let source_extensions = source_pack
        .get("extensions")
        .and_then(|value| value.as_mapping())
        .ok_or_else(|| anyhow::anyhow!("source pack.yaml missing extensions"))?;

    if source.extensions.include.is_empty() {
        return Err(anyhow::anyhow!(
            "source.extensions.include must list extension keys to copy"
        ));
    }
    if !source
        .extensions
        .include
        .iter()
        .any(|key| key == PROVIDER_EXTENSION_ID)
    {
        return Err(anyhow::anyhow!(
            "source.extensions.include must include {}",
            PROVIDER_EXTENSION_ID
        ));
    }
    for key in &source.extensions.include {
        if !source_extensions.contains_key(serde_yaml::Value::String(key.clone())) {
            return Err(anyhow::anyhow!("source pack missing extension '{}'", key));
        }
    }

    let provider_entry = find_provider_entry(&source_pack, &spec.provider.provider_type)?;
    validate_provider_entry(spec, provider_entry)?;

    let component_worlds = source_component_worlds(&source_pack);
    if let Some(ingress) = &spec.components.ingress
        && !component_worlds.contains(&ingress.world)
    {
        return Err(anyhow::anyhow!(
            "components.ingress.world '{}' not found in source pack components",
            ingress.world
        ));
    }
    if let Some(subscriptions) = &spec.components.subscriptions
        && !component_worlds.contains(&subscriptions.world)
    {
        return Err(anyhow::anyhow!(
            "components.subscriptions.world '{}' not found in source pack components",
            subscriptions.world
        ));
    }

    Ok(())
}

fn validate_contract(spec: &Spec, contract: &ContractSpec, flows: &[String]) -> Result<()> {
    if let Some(ingress) = &contract.ingress {
        let allowed = ["none", "default", "custom"];
        if !allowed.contains(&ingress.mode.as_str()) {
            return Err(anyhow::anyhow!(
                "contract.ingress.mode must be one of {:?}",
                allowed
            ));
        }
        if ingress.mode == "custom" && spec.components.ingress.is_none() {
            return Err(anyhow::anyhow!(
                "contract.ingress.mode=custom requires components.ingress"
            ));
        }
        if ingress.mode == "custom"
            && let Some(source) = &spec.source
            && !source
                .extensions
                .include
                .iter()
                .any(|key| key == "messaging.provider_ingress.v1")
        {
            return Err(anyhow::anyhow!(
                "contract.ingress.mode=custom requires messaging.provider_ingress.v1 extension"
            ));
        }
    }
    if let Some(webhooks) = &contract.webhooks
        && webhooks.required
        && !flows.iter().any(|flow| flow == "verify_webhooks")
    {
        return Err(anyhow::anyhow!(
            "contract.webhooks.required=true requires verify_webhooks flow"
        ));
    }
    if let Some(subscriptions) = &contract.subscriptions {
        if subscriptions.required && !flows.iter().any(|flow| flow == "sync_subscriptions") {
            return Err(anyhow::anyhow!(
                "contract.subscriptions.required=true requires sync_subscriptions flow"
            ));
        }
        if subscriptions.required
            && let Some(source) = &spec.source
            && !source
                .extensions
                .include
                .iter()
                .any(|key| key == "messaging.subscriptions.v1")
        {
            return Err(anyhow::anyhow!(
                "contract.subscriptions.required=true requires messaging.subscriptions.v1 extension"
            ));
        }
    }
    if let Some(render) = &contract.render
        && render.required
        && spec.components.renderer.is_none()
    {
        return Err(anyhow::anyhow!(
            "contract.render.required=true requires components.renderer"
        ));
    }
    Ok(())
}

fn known_worlds_from_interfaces() -> Result<BTreeSet<String>> {
    let crate_dir = find_registry_crate_dir("greentic-interfaces")?;
    let wit_root = crate_dir.join("wit");
    let mut packages = Vec::new();
    collect_package_wits(&wit_root, &mut packages)?;
    let mut worlds = BTreeSet::new();
    for package_wit in packages {
        let (package_id, world_names) = parse_package_wit(&package_wit)?;
        for world in world_names {
            let full = format!("{}/{}", package_id, world);
            worlds.insert(full);
            if let Some(alias) = provider_schema_core_alias(&package_id, &world) {
                worlds.insert(alias);
            }
        }
    }
    if worlds.is_empty() {
        return Err(anyhow::anyhow!(
            "no WIT worlds found under {}",
            wit_root.display()
        ));
    }
    Ok(worlds)
}

fn provider_schema_core_alias(package_id: &str, world: &str) -> Option<String> {
    if world != "schema-core" {
        return None;
    }
    let mut parts = package_id.split('@');
    let name = parts.next()?;
    let version = parts.next()?;
    if name != "greentic:provider-schema-core" {
        return None;
    }
    Some(format!("greentic:provider/schema-core@{}", version))
}

fn known_render_tiers_from_interfaces() -> Result<BTreeSet<String>> {
    let crate_dir = find_registry_crate_dir("greentic-interfaces")?;
    let world_wit = crate_dir
        .join("wit")
        .join("provider-common")
        .join("world.wit");
    let contents = fs::read_to_string(&world_wit)
        .with_context(|| format!("reading {}", world_wit.display()))?;
    let mut tiers = BTreeSet::new();
    let mut in_enum = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("enum render-tier") || trimmed.starts_with("enum tier") {
            in_enum = true;
            continue;
        }
        if in_enum {
            if trimmed.starts_with('}') {
                break;
            }
            let value = trimmed.trim_end_matches(',').trim();
            if !value.is_empty() {
                tiers.insert(value.to_string());
            }
        }
    }
    if tiers.is_empty() {
        return Err(anyhow::anyhow!(
            "render-tier enum not found in {}",
            world_wit.display()
        ));
    }
    Ok(tiers)
}

fn find_registry_crate_dir(crate_name: &str) -> Result<PathBuf> {
    let cargo_home = env::var("CARGO_HOME").unwrap_or_else(|_| {
        env::var("HOME")
            .map(|home| PathBuf::from(home).join(".cargo"))
            .unwrap_or_else(|_| PathBuf::from(".cargo"))
            .to_string_lossy()
            .to_string()
    });
    let registry_src = PathBuf::from(cargo_home).join("registry").join("src");
    if !registry_src.exists() {
        return Err(anyhow::anyhow!(
            "cargo registry not found at {}",
            registry_src.display()
        ));
    }
    let mut best: Option<(VersionTriple, PathBuf)> = None;
    for registry in fs::read_dir(&registry_src)? {
        let registry = registry?;
        if !registry.path().is_dir() {
            continue;
        }
        for entry in fs::read_dir(registry.path())? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let prefix = format!("{crate_name}-");
            if !name.starts_with(&prefix) {
                continue;
            }
            let version_str = &name[prefix.len()..];
            let Some(version) = VersionTriple::parse(version_str) else {
                continue;
            };
            match &best {
                Some((best_version, _)) if best_version >= &version => {}
                _ => {
                    best = Some((version, entry.path()));
                }
            }
        }
    }
    best.map(|(_, path)| path).ok_or_else(|| {
        anyhow::anyhow!(
            "could not locate {} in cargo registry (expected under {})",
            crate_name,
            registry_src.display()
        )
    })
}

fn collect_package_wits(dir: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_package_wits(&path, output)?;
        } else if path.file_name().and_then(|f| f.to_str()) == Some("package.wit") {
            output.push(path);
        }
    }
    Ok(())
}

fn parse_package_wit(path: &Path) -> Result<(String, Vec<String>)> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut package_id = None;
    let mut worlds = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("package ") {
            let value = trimmed
                .trim_start_matches("package ")
                .trim_end_matches(';')
                .trim();
            if !value.is_empty() {
                package_id = Some(value.to_string());
            }
        }
        if trimmed.starts_with("world ") {
            let value = trimmed
                .trim_start_matches("world ")
                .split_whitespace()
                .next()
                .unwrap_or("");
            if !value.is_empty() {
                worlds.push(value.to_string());
            }
        }
    }
    let package_id =
        package_id.ok_or_else(|| anyhow::anyhow!("missing package id in {}", path.display()))?;
    Ok((package_id, worlds))
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct VersionTriple(u64, u64, u64);

impl VersionTriple {
    fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some(Self(major, minor, patch))
    }
}

fn write_default_spec_file(path: &Path) -> Result<()> {
    let spec = Spec {
        pack: PackSpec {
            id: "messaging-dummy".to_string(),
            name: Some("Messaging Dummy".to_string()),
            version: default_version(),
            publisher: Some("Greentic".to_string()),
            kind: Some("application".to_string()),
        },
        provider: ProviderSpec {
            provider_type: "messaging.dummy".to_string(),
            provider_id: Some("dummy".to_string()),
            ops: default_ops(),
            capabilities: json!({ "render_tiers": ["tier-d"] }),
            config_schema_ref: None,
            state_schema_ref: None,
            docs_ref: None,
        },
        components: ComponentsSpec {
            adapter: ComponentSpec {
                component_ref:
                    "local:../components/messaging-provider-dummy/messaging-provider-dummy.wasm"
                        .to_string(),
                world: "greentic:provider/schema-core@1.0.0".to_string(),
                export: Some("schema-core-api".to_string()),
                id: None,
                manifest: Some(PathBuf::from(
                    "../components/messaging-provider-dummy/component.manifest.json",
                )),
            },
            renderer: None,
            ingress: None,
            subscriptions: None,
        },
        flows: FlowsSpec {
            generate: vec![
                "setup_default".to_string(),
                "diagnostics".to_string(),
                "requirements".to_string(),
            ],
            include: Vec::new(),
        },
        setup: Some(SetupSpec {
            questions: vec![SetupQuestionSpec {
                key: "public_base_url".to_string(),
                prompt: "Public base URL".to_string(),
                kind: "string".to_string(),
                required: Some(false),
                help: Some("Example: https://xxxx.trycloudflare.com".to_string()),
                choices: None,
                default: None,
                validate: None,
                write_to: "config:public_base_url".to_string(),
            }],
            emits_success_message: Some(true),
            asset_path: None,
        }),
        requirements: Some(RequirementsSpec {
            required_args: vec!["destination".to_string()],
            optional_args: Vec::new(),
            examples: vec![RequirementsExample {
                args: json!({ "destination": "dummy" }),
                text: "Hello from dummy".to_string(),
            }],
            notes: Some("Dummy provider requires a destination for egress-only tests.".to_string()),
        }),
        validators: Some(vec![ValidatorSpec {
            id: "greentic.validators.messaging".to_string(),
            component_ref: "oci://ghcr.io/greentic-ai/validators/messaging:latest".to_string(),
        }]),
        source: None,
        contract: None,
    };
    let yaml = serde_yaml::to_string(&spec)?;
    fs::write(path, yaml).with_context(|| format!("writing default spec {}", path.display()))?;
    Ok(())
}

fn provider_id(spec: &Spec) -> String {
    if let Some(provider_id) = &spec.provider.provider_id {
        return provider_id.clone();
    }
    spec.provider
        .provider_type
        .split('.')
        .next_back()
        .unwrap_or(&spec.pack.id)
        .to_string()
}

fn write_setup_yaml(path: &Path, spec: &Spec) -> Result<()> {
    let setup = spec
        .setup
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("setup section missing"))?;

    let questions: Vec<serde_yaml::Value> = setup
        .questions
        .iter()
        .map(|question| {
            let mut map = serde_yaml::Mapping::new();
            map.insert(
                serde_yaml::Value::String("name".to_string()),
                serde_yaml::Value::String(question.key.clone()),
            );
            map.insert(
                serde_yaml::Value::String("title".to_string()),
                serde_yaml::Value::String(question.prompt.clone()),
            );
            map.insert(
                serde_yaml::Value::String("kind".to_string()),
                serde_yaml::Value::String(question.kind.clone()),
            );
            if let Some(required) = question.required {
                map.insert(
                    serde_yaml::Value::String("required".to_string()),
                    serde_yaml::Value::Bool(required),
                );
            }
            if let Some(help) = &question.help {
                map.insert(
                    serde_yaml::Value::String("help".to_string()),
                    serde_yaml::Value::String(help.clone()),
                );
            }
            if let Some(choices) = &question.choices {
                let values = choices
                    .iter()
                    .map(|choice| serde_yaml::Value::String(choice.clone()))
                    .collect::<Vec<_>>();
                map.insert(
                    serde_yaml::Value::String("choices".to_string()),
                    serde_yaml::Value::Sequence(values),
                );
            }
            if let Some(default) = &question.default {
                let value = serde_yaml::to_value(default).unwrap_or(serde_yaml::Value::Null);
                map.insert(serde_yaml::Value::String("default".to_string()), value);
            }
            if let Some(validate) = &question.validate {
                let value = serde_yaml::to_value(validate).unwrap_or(serde_yaml::Value::Null);
                map.insert(serde_yaml::Value::String("validate".to_string()), value);
            }
            if question.write_to.trim_start().starts_with("secrets:") {
                map.insert(
                    serde_yaml::Value::String("secret".to_string()),
                    serde_yaml::Value::Bool(true),
                );
            }
            serde_yaml::Value::Mapping(map)
        })
        .collect();

    let mut payload = serde_yaml::Mapping::new();
    payload.insert(
        serde_yaml::Value::String("provider_id".to_string()),
        serde_yaml::Value::String(provider_id(spec)),
    );
    payload.insert(
        serde_yaml::Value::String("version".to_string()),
        serde_yaml::Value::Number(serde_yaml::Number::from(1)),
    );
    payload.insert(
        serde_yaml::Value::String("title".to_string()),
        serde_yaml::Value::String(format!("{} provider setup", provider_id(spec))),
    );
    payload.insert(
        serde_yaml::Value::String("questions".to_string()),
        serde_yaml::Value::Sequence(questions),
    );

    let contents = serde_yaml::to_string(&serde_yaml::Value::Mapping(payload))?;
    fs::write(path, contents).with_context(|| format!("writing setup.yaml {}", path.display()))?;
    Ok(())
}

fn run_flow_doctor(flow_path: &Path) -> Result<()> {
    let status = Command::new("greentic-flow")
        .arg("doctor")
        .arg(flow_path)
        .status()
        .with_context(|| format!("running greentic-flow doctor on {}", flow_path.display()))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "greentic-flow doctor failed for {}",
            flow_path.display()
        ));
    }
    Ok(())
}

fn pack_new(out_dir: &Path, pack_id: &str) -> Result<()> {
    let status = Command::new(greentic_pack_bin())
        .args(["new", "--dir"])
        .arg(out_dir)
        .arg(pack_id)
        .status()
        .with_context(|| format!("running greentic-pack new at {}", out_dir.display()))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "greentic-pack new failed for {}",
            out_dir.display()
        ));
    }
    Ok(())
}

fn pack_update(out_dir: &Path) -> Result<()> {
    let status = Command::new(greentic_pack_bin())
        .args(["update", "--in"])
        .arg(out_dir)
        .status()
        .with_context(|| format!("running greentic-pack update at {}", out_dir.display()))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "greentic-pack update failed for {}",
            out_dir.display()
        ));
    }
    Ok(())
}

fn add_provider_extension(
    pack_dir: &Path,
    provider_id: &str,
    kind: &str,
    validator: Option<&ValidatorSpec>,
) -> Result<()> {
    let mut cmd = Command::new(greentic_pack_bin());
    cmd.args([
        "add-extension",
        "provider",
        "--pack-dir",
        pack_dir
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid pack dir"))?,
        "--id",
        provider_id,
        "--kind",
        kind,
        "--route",
        provider_id,
        "--flow",
        "setup_default",
    ]);
    if let Some(validator) = validator {
        cmd.args(["--validator-ref", &validator.component_ref]);
    }
    let status = cmd.status().with_context(|| {
        format!(
            "running greentic-pack add-extension in {}",
            pack_dir.display()
        )
    })?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "greentic-pack add-extension failed for {}",
            pack_dir.display()
        ));
    }
    Ok(())
}

fn update_pack_yaml(
    out_dir: &Path,
    spec: &Spec,
    source_pack_yaml: Option<&Path>,
    merge_components: bool,
) -> Result<()> {
    let pack_yaml_path = out_dir.join("pack.yaml");
    let contents = fs::read_to_string(&pack_yaml_path)
        .with_context(|| format!("reading {}", pack_yaml_path.display()))?;
    let mut pack: serde_yaml::Value =
        serde_yaml::from_str(&contents).context("parsing pack.yaml")?;

    let mapping = pack
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("pack.yaml is not a mapping"))?;

    let kind = spec
        .pack
        .kind
        .clone()
        .unwrap_or_else(|| "application".to_string());
    mapping.insert(
        serde_yaml::Value::String("pack_id".to_string()),
        serde_yaml::Value::String(spec.pack.id.clone()),
    );
    mapping.insert(
        serde_yaml::Value::String("version".to_string()),
        serde_yaml::Value::String(spec.pack.version.clone()),
    );
    mapping.insert(
        serde_yaml::Value::String("kind".to_string()),
        serde_yaml::Value::String(kind),
    );
    if let Some(publisher) = &spec.pack.publisher {
        mapping.insert(
            serde_yaml::Value::String("publisher".to_string()),
            serde_yaml::Value::String(publisher.clone()),
        );
    }

    if let Some(source_pack_yaml) = source_pack_yaml {
        let source_pack = load_pack_yaml(source_pack_yaml)?;
        let source_entrypoints = flow_entrypoints_from_pack(&source_pack);
        update_flow_entrypoints(mapping, &source_entrypoints)?;
        merge_extensions_from_source(mapping, &source_pack, &spec.source)?;
        if merge_components {
            merge_components_from_source(mapping, &source_pack)?;
        }
    } else {
        update_flow_entrypoints(mapping, &BTreeSet::new())?;
    }

    let updated = serde_yaml::to_string(&pack)?;
    fs::write(&pack_yaml_path, updated)
        .with_context(|| format!("writing {}", pack_yaml_path.display()))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn generate_from_source(
    spec_path: &Path,
    out_dir: &Path,
    source: &SourceSpec,
    flows: &[String],
    flows_dir: &Path,
    components_dir: &Path,
    assets_dir: &Path,
    spec: &Spec,
) -> Result<()> {
    let spec_dir = spec_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("spec has no parent directory"))?;
    let source_pack_dir = resolve_local_path(spec_dir, &source.pack_dir)?;
    let source_pack_yaml = source_pack_dir.join("pack.yaml");

    remove_default_flow(flows_dir)?;
    copy_supporting_from_source(&source_pack_dir, out_dir, assets_dir, source)?;
    if !out_dir.join("secret-requirements.json").exists() {
        write_empty_secret_requirements(out_dir)?;
    }

    let answers_dir = out_dir.join(".packgen-answers");
    fs::create_dir_all(&answers_dir)?;
    stage_components(components_dir, spec_path, spec, flows)?;
    generate_flows(flows_dir, &answers_dir, spec_path, spec, flows)?;
    fs::remove_dir_all(&answers_dir).ok();

    for flow_name in flows {
        run_flow_doctor(&flows_dir.join(format!("{flow_name}.ygtc")))?;
    }

    pack_update(out_dir)?;

    let source_pack = load_pack_yaml(&source_pack_yaml)?;
    let validator = spec
        .validators
        .as_ref()
        .and_then(|v| v.first())
        .cloned()
        .or_else(|| validator_from_source(&source_pack));
    add_provider_extension(
        out_dir,
        &spec.provider.provider_type,
        "messaging",
        validator.as_ref(),
    )?;

    update_pack_yaml(out_dir, spec, Some(&source_pack_yaml), false)?;
    update_pack_manifest(out_dir)?;
    verify_pack_dir(out_dir, spec, flows)?;

    Ok(())
}

fn copy_supporting_from_source(
    source_pack_dir: &Path,
    out_dir: &Path,
    assets_dir: &Path,
    source: &SourceSpec,
) -> Result<()> {
    let copy_spec = source.copy.as_ref();
    let copy_assets = copy_spec.and_then(|spec| spec.assets).unwrap_or(true);
    let copy_fixtures = copy_spec.and_then(|spec| spec.fixtures).unwrap_or(true);
    let copy_schemas = copy_spec.and_then(|spec| spec.schemas).unwrap_or(true);
    let root_files = copy_spec
        .and_then(|spec| spec.root_files.clone())
        .unwrap_or_else(default_root_files);

    fs::create_dir_all(out_dir)?;
    fs::create_dir_all(assets_dir)?;

    if copy_assets {
        copy_dir_if_exists(&source_pack_dir.join("assets"), assets_dir)?;
    }
    if copy_fixtures {
        copy_dir_if_exists(&source_pack_dir.join("fixtures"), &out_dir.join("fixtures"))?;
    }
    if copy_schemas {
        copy_dir_if_exists(&source_pack_dir.join("schemas"), &out_dir.join("schemas"))?;
    }
    for file in root_files {
        let src = source_pack_dir.join(&file);
        if src.exists() {
            copy_file(&src, &out_dir.join(&file))?;
        }
    }
    Ok(())
}

fn default_root_files() -> Vec<String> {
    vec![
        "setup.yaml".to_string(),
        "secret-requirements.json".to_string(),
        ".secret_requirements.json".to_string(),
    ]
}

fn flow_files_for(flow_dir: &Path, flow_id: &str) -> Result<Vec<PathBuf>> {
    let mut matches = Vec::new();
    if !flow_dir.exists() {
        return Ok(matches);
    }
    for entry in fs::read_dir(flow_dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if name.starts_with(flow_id) && name.contains(".ygtc") {
            matches.push(path);
        }
    }
    Ok(matches)
}

fn load_pack_yaml(path: &Path) -> Result<serde_yaml::Value> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let pack: serde_yaml::Value = serde_yaml::from_str(&contents).context("parsing pack.yaml")?;
    Ok(pack)
}

fn flow_entrypoints_from_pack(pack: &serde_yaml::Value) -> BTreeSet<(String, Vec<String>)> {
    let mut entrypoints = BTreeSet::new();
    let flows = pack
        .get("flows")
        .and_then(|value| value.as_sequence())
        .cloned()
        .unwrap_or_default();
    for flow in flows {
        let Some(flow_map) = flow.as_mapping() else {
            continue;
        };
        let Some(id) = flow_map
            .get(serde_yaml::Value::String("id".to_string()))
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        let entry = flow_map
            .get(serde_yaml::Value::String("entrypoints".to_string()))
            .and_then(|value| value.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|value| value.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        entrypoints.insert((id.to_string(), entry));
    }
    entrypoints
}

fn source_component_worlds(pack: &serde_yaml::Value) -> BTreeSet<String> {
    let mut worlds = BTreeSet::new();
    let components = pack
        .get("components")
        .and_then(|value| value.as_sequence())
        .cloned()
        .unwrap_or_default();
    for component in components {
        let Some(component_map) = component.as_mapping() else {
            continue;
        };
        if let Some(world) = component_map
            .get(serde_yaml::Value::String("world".to_string()))
            .and_then(|value| value.as_str())
        {
            worlds.insert(world.to_string());
        }
    }
    worlds
}

fn merge_extensions_from_source(
    mapping: &mut serde_yaml::Mapping,
    source_pack: &serde_yaml::Value,
    source_spec: &Option<SourceSpec>,
) -> Result<()> {
    let Some(source_spec) = source_spec else {
        return Ok(());
    };
    let Some(source_extensions) = source_pack
        .get("extensions")
        .and_then(|value| value.as_mapping())
    else {
        return Ok(());
    };
    let extensions = mapping
        .entry(serde_yaml::Value::String("extensions".to_string()))
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    let Some(dest_extensions) = extensions.as_mapping_mut() else {
        return Err(anyhow::anyhow!("pack.yaml extensions is not a mapping"));
    };
    for key in &source_spec.extensions.include {
        let Some(value) = source_extensions.get(serde_yaml::Value::String(key.clone())) else {
            continue;
        };
        dest_extensions.insert(serde_yaml::Value::String(key.clone()), value.clone());
    }
    Ok(())
}

fn merge_components_from_source(
    mapping: &mut serde_yaml::Mapping,
    source_pack: &serde_yaml::Value,
) -> Result<()> {
    let Some(source_components) = source_pack
        .get("components")
        .and_then(|value| value.as_sequence())
    else {
        return Ok(());
    };
    mapping.insert(
        serde_yaml::Value::String("components".to_string()),
        serde_yaml::Value::Sequence(source_components.clone()),
    );
    Ok(())
}

fn validator_from_source(source_pack: &serde_yaml::Value) -> Option<ValidatorSpec> {
    let extensions = source_pack.get("extensions")?.as_mapping()?;
    let validators = extensions
        .get(serde_yaml::Value::String(
            "greentic.messaging.validators.v1".to_string(),
        ))?
        .as_mapping()?;
    let inline = validators
        .get(serde_yaml::Value::String("inline".to_string()))?
        .as_mapping()?;
    let validators_list = inline
        .get(serde_yaml::Value::String("validators".to_string()))?
        .as_sequence()?;
    let first = validators_list.first()?.as_mapping()?;
    let id = first
        .get(serde_yaml::Value::String("id".to_string()))?
        .as_str()?;
    let component_ref = first
        .get(serde_yaml::Value::String("component_ref".to_string()))?
        .as_str()?;
    Some(ValidatorSpec {
        id: id.to_string(),
        component_ref: component_ref.to_string(),
    })
}

fn find_provider_entry<'a>(
    pack: &'a serde_yaml::Value,
    provider_type: &str,
) -> Result<&'a serde_yaml::Mapping> {
    let extensions = pack
        .get("extensions")
        .and_then(|value| value.as_mapping())
        .ok_or_else(|| anyhow::anyhow!("pack.yaml missing extensions"))?;
    let provider_extension = extensions
        .get(serde_yaml::Value::String(PROVIDER_EXTENSION_ID.to_string()))
        .and_then(|value| value.as_mapping())
        .ok_or_else(|| anyhow::anyhow!("pack.yaml missing {}", PROVIDER_EXTENSION_ID))?;
    let inline = provider_extension
        .get(serde_yaml::Value::String("inline".to_string()))
        .and_then(|value| value.as_mapping())
        .ok_or_else(|| anyhow::anyhow!("{} inline block missing", PROVIDER_EXTENSION_ID))?;
    let providers = inline
        .get(serde_yaml::Value::String("providers".to_string()))
        .and_then(|value| value.as_sequence())
        .ok_or_else(|| anyhow::anyhow!("{} providers missing", PROVIDER_EXTENSION_ID))?;
    for provider in providers {
        let Some(provider_map) = provider.as_mapping() else {
            continue;
        };
        let Some(existing_type) = provider_map
            .get(serde_yaml::Value::String("provider_type".to_string()))
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        if existing_type == provider_type {
            return Ok(provider_map);
        }
    }
    Err(anyhow::anyhow!(
        "{} missing provider_type '{}'",
        PROVIDER_EXTENSION_ID,
        provider_type
    ))
}

fn validate_provider_entry(spec: &Spec, provider_entry: &serde_yaml::Mapping) -> Result<()> {
    let ops = provider_entry
        .get(serde_yaml::Value::String("ops".to_string()))
        .and_then(|value| value.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|value| value.as_str().map(|s| s.to_string()))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let spec_ops: BTreeSet<_> = spec.provider.ops.iter().cloned().collect();
    if ops != spec_ops {
        return Err(anyhow::anyhow!(
            "provider ops mismatch. spec={spec_ops:?} source={ops:?}"
        ));
    }

    let runtime = provider_entry
        .get(serde_yaml::Value::String("runtime".to_string()))
        .and_then(|value| value.as_mapping())
        .ok_or_else(|| anyhow::anyhow!("provider runtime missing"))?;
    let component_ref = runtime
        .get(serde_yaml::Value::String("component_ref".to_string()))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let export = runtime
        .get(serde_yaml::Value::String("export".to_string()))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let world = runtime
        .get(serde_yaml::Value::String("world".to_string()))
        .and_then(|value| value.as_str())
        .unwrap_or("");

    if let Some(expected_id) = &spec.components.adapter.id
        && component_ref != expected_id
    {
        return Err(anyhow::anyhow!(
            "provider runtime component_ref '{}' does not match spec adapter id '{}'",
            component_ref,
            expected_id
        ));
    }
    if spec.components.adapter.id.is_none() && !component_ref.is_empty() {
        // Skip strict id check if not provided in spec.
    }
    if let Some(expected) = &spec.components.adapter.export
        && export != expected
    {
        return Err(anyhow::anyhow!(
            "provider runtime export '{}' does not match spec adapter export '{}'",
            export,
            expected
        ));
    }
    if world != spec.components.adapter.world {
        return Err(anyhow::anyhow!(
            "provider runtime world '{}' does not match spec adapter world '{}'",
            world,
            spec.components.adapter.world
        ));
    }

    Ok(())
}

fn update_pack_manifest(out_dir: &Path) -> Result<()> {
    let script = workspace_root()?
        .join("tools")
        .join("generate_pack_metadata.py");
    let manifest_path = out_dir.join("pack.manifest.json");
    if !manifest_path.exists() {
        fs::write(&manifest_path, "{}\n")
            .with_context(|| format!("writing {}", manifest_path.display()))?;
    }
    let status = Command::new("python3")
        .arg(script)
        .args(["--pack-dir"])
        .arg(out_dir)
        .args(["--components-dir"])
        .arg(out_dir.join("components"))
        .args(["--include-capabilities-cache"])
        .args(["--secrets-out"])
        .arg(out_dir.join("secret-requirements.json"))
        .status()
        .with_context(|| format!("running pack manifest update on {}", out_dir.display()))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "pack manifest update failed for {}",
            out_dir.display()
        ));
    }
    Ok(())
}

fn verify_pack_dir(out_dir: &Path, spec: &Spec, flows: &[String]) -> Result<()> {
    let pack_yaml_path = out_dir.join("pack.yaml");
    let contents = fs::read_to_string(&pack_yaml_path)
        .with_context(|| format!("reading {}", pack_yaml_path.display()))?;
    let mut pack: serde_yaml::Value =
        serde_yaml::from_str(&contents).context("parsing pack.yaml")?;
    let mapping = pack
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("pack.yaml is not a mapping"))?;

    let pack_id = mapping
        .get(serde_yaml::Value::String("pack_id".to_string()))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if pack_id != spec.pack.id {
        return Err(anyhow::anyhow!(
            "pack.yaml pack_id '{}' does not match spec.pack.id '{}'",
            pack_id,
            spec.pack.id
        ));
    }

    let flows_value = mapping
        .get(serde_yaml::Value::String("flows".to_string()))
        .ok_or_else(|| anyhow::anyhow!("pack.yaml missing flows section"))?;
    let pack_flows = flows_value
        .as_sequence()
        .ok_or_else(|| anyhow::anyhow!("pack.yaml flows section is not a list"))?;
    let mut flow_entrypoints = BTreeSet::new();
    for flow in pack_flows {
        let Some(flow_map) = flow.as_mapping() else {
            continue;
        };
        let Some(id) = flow_map
            .get(serde_yaml::Value::String("id".to_string()))
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        let entrypoints = flow_map
            .get(serde_yaml::Value::String("entrypoints".to_string()))
            .and_then(|value| value.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        flow_entrypoints.insert((id.to_string(), entrypoints));
    }

    for flow in flows {
        let entrypoints = flow_entrypoints
            .iter()
            .find(|(id, _)| id == flow)
            .map(|(_, entrypoints)| entrypoints)
            .ok_or_else(|| anyhow::anyhow!("pack.yaml missing flow '{}'", flow))?;
        if spec.source.is_none()
            && let Some(expected) = expected_entrypoint(flow)
            && !entrypoints.iter().any(|value| value == expected)
        {
            return Err(anyhow::anyhow!(
                "flow '{}' missing entrypoint '{}'",
                flow,
                expected
            ));
        }
    }

    let extensions = mapping
        .get(serde_yaml::Value::String("extensions".to_string()))
        .and_then(|value| value.as_mapping())
        .ok_or_else(|| anyhow::anyhow!("pack.yaml missing extensions section"))?;
    let provider_extension = extensions
        .get(serde_yaml::Value::String(PROVIDER_EXTENSION_ID.to_string()))
        .and_then(|value| value.as_mapping())
        .ok_or_else(|| anyhow::anyhow!("pack.yaml missing {}", PROVIDER_EXTENSION_ID))?;
    let inline = provider_extension
        .get(serde_yaml::Value::String("inline".to_string()))
        .and_then(|value| value.as_mapping())
        .ok_or_else(|| anyhow::anyhow!("{} inline block missing", PROVIDER_EXTENSION_ID))?;
    let providers = inline
        .get(serde_yaml::Value::String("providers".to_string()))
        .and_then(|value| value.as_sequence())
        .ok_or_else(|| anyhow::anyhow!("{} providers missing", PROVIDER_EXTENSION_ID))?;
    let mut has_provider = false;
    for provider in providers {
        let Some(provider_map) = provider.as_mapping() else {
            continue;
        };
        let Some(provider_type) = provider_map
            .get(serde_yaml::Value::String("provider_type".to_string()))
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        if provider_type == spec.provider.provider_type {
            has_provider = true;
        }
    }
    if !has_provider {
        return Err(anyhow::anyhow!(
            "{} missing provider_type '{}'",
            PROVIDER_EXTENSION_ID,
            spec.provider.provider_type
        ));
    }

    Ok(())
}
fn update_flow_entrypoints(
    mapping: &mut serde_yaml::Mapping,
    source_entrypoints: &BTreeSet<(String, Vec<String>)>,
) -> Result<()> {
    let flows_value = mapping
        .get_mut(serde_yaml::Value::String("flows".to_string()))
        .ok_or_else(|| anyhow::anyhow!("pack.yaml missing flows section"))?;
    let flows = flows_value
        .as_sequence_mut()
        .ok_or_else(|| anyhow::anyhow!("pack.yaml flows section is not a list"))?;
    for flow in flows {
        let Some(flow_map) = flow.as_mapping_mut() else {
            continue;
        };
        let Some(id) = flow_map
            .get(serde_yaml::Value::String("id".to_string()))
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        let mut entrypoints = source_entrypoints
            .iter()
            .find(|(source_id, _)| source_id == id)
            .map(|(_, values)| values.clone())
            .unwrap_or_default();
        if entrypoints.is_empty() {
            entrypoints = expected_entrypoint(id)
                .map(|value| vec![value.to_string()])
                .unwrap_or_default();
        }
        if entrypoints.is_empty() {
            continue;
        }
        flow_map.insert(
            serde_yaml::Value::String("entrypoints".to_string()),
            serde_yaml::Value::Sequence(
                entrypoints
                    .into_iter()
                    .map(serde_yaml::Value::String)
                    .collect(),
            ),
        );
    }
    Ok(())
}

fn expected_entrypoint(flow_id: &str) -> Option<&'static str> {
    match flow_id {
        "setup_default" | "setup_custom" => Some("setup"),
        "diagnostics" => Some("diagnostics"),
        "requirements" => Some("requirements"),
        "verify_webhooks" => Some("verify_webhooks"),
        "sync_subscriptions" => Some("subscriptions"),
        "rotate_credentials" => Some("rotate_credentials"),
        _ => None,
    }
}

fn config_schema_ref(spec: &Spec) -> String {
    if let Some(value) = &spec.provider.config_schema_ref {
        return value.clone();
    }
    format!(
        "schemas/messaging/{}/public.config.schema.json",
        provider_id(spec)
    )
}

fn greentic_pack_bin() -> String {
    std::env::var("GREENTIC_PACK_BIN").unwrap_or_else(|_| "greentic-pack".to_string())
}

fn remove_default_flow(flows_dir: &Path) -> Result<()> {
    let default_flow = flows_dir.join("main.ygtc");
    if default_flow.exists() {
        fs::remove_file(&default_flow)
            .with_context(|| format!("removing {}", default_flow.display()))?;
    }
    Ok(())
}

fn generate_flows(
    flows_dir: &Path,
    answers_dir: &Path,
    spec_path: &Path,
    spec: &Spec,
    flows: &[String],
) -> Result<()> {
    let root = workspace_root()?;
    let templates_manifest = root.join("components/templates/component.manifest.json");
    let questions_manifest = root.join("components/questions/component.manifest.json");
    let provision_manifest = root.join("components/provision/component.manifest.json");

    let generated_meta = generated_flow_metadata(
        spec_path,
        &[
            templates_manifest.clone(),
            questions_manifest.clone(),
            provision_manifest.clone(),
        ],
    )?;

    let templates_manifest_inline = inline_manifest(
        &templates_manifest,
        answers_dir,
        "templates.inline.manifest.json",
    )?;
    let questions_manifest_inline = inline_manifest(
        &questions_manifest,
        answers_dir,
        "questions.inline.manifest.json",
    )?;
    let provision_manifest_inline = inline_manifest(
        &provision_manifest,
        answers_dir,
        "provision.inline.manifest.json",
    )?;

    let templates_example_raw =
        answers_example(&templates_manifest, "text", "templates_text", answers_dir)?;
    let templates_example = merge_payload(
        templates_example_raw,
        base_templates_payload(&spec.provider.provider_type),
    );
    let questions_emit_example =
        answers_example(&questions_manifest, "emit", "questions_emit", answers_dir)?;
    let questions_validate_example = answers_example(
        &questions_manifest,
        "validate",
        "questions_validate",
        answers_dir,
    )?;
    let provision_apply_example =
        answers_example(&provision_manifest, "apply", "provision_apply", answers_dir)?;

    for flow in flows {
        match flow.as_str() {
            "diagnostics" => {
                let diagnostics = flows_dir.join("diagnostics.ygtc");
                flow_new(&diagnostics, "diagnostics", "job")?;
                let summary_text = diagnostics_summary(spec);
                let payload = merge_payload(
                    templates_example.clone(),
                    json!({
                        "template": summary_text,
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &diagnostics,
                    "summary",
                    "text",
                    payload,
                    Some("out"),
                    None,
                    None,
                    &templates_manifest_inline,
                    "../components/templates/templates.wasm",
                )?;
                stamp_generated_header(&diagnostics, &generated_meta)?;
            }
            "requirements" => {
                let requirements = flows_dir.join("requirements.ygtc");
                flow_new(&requirements, "requirements", "job")?;
                let requirements_text = requirements_payload(spec)?;
                let payload = merge_payload(
                    templates_example.clone(),
                    json!({
                        "template": requirements_text,
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &requirements,
                    "summary",
                    "text",
                    payload,
                    Some("out"),
                    None,
                    None,
                    &templates_manifest_inline,
                    "../components/templates/templates.wasm",
                )?;
                stamp_generated_header(&requirements, &generated_meta)?;
            }
            "setup_default" => {
                let setup_default = flows_dir.join("setup_default.ygtc");
                flow_new(&setup_default, "setup_default", "job")?;
                let emit_payload = merge_payload(
                    questions_emit_example.clone(),
                    json!({
                        "id": format!("{}-setup_default", provider_id(spec)),
                        "spec_ref": "assets/setup.yaml"
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &setup_default,
                    "setup_default__emit_questions",
                    "emit",
                    emit_payload,
                    Some("out"),
                    None,
                    None,
                    &questions_manifest_inline,
                    "../components/questions/questions.wasm",
                )?;

                let collect_payload = merge_payload(
                    templates_example.clone(),
                    json!({
                        "template": "Collect inputs for setup_default.",
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &setup_default,
                    "setup_default__collect",
                    "text",
                    collect_payload,
                    Some("out"),
                    None,
                    Some("setup_default__emit_questions"),
                    &templates_manifest_inline,
                    "../components/templates/templates.wasm",
                )?;

                let validate_payload = merge_payload(
                    questions_validate_example.clone(),
                    json!({
                        "answers_json": "{{ state.input.answers_json }}",
                        "spec_json": "{{ node.setup_default__emit_questions }}"
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &setup_default,
                    "setup_default__validate",
                    "validate",
                    validate_payload,
                    Some("out"),
                    None,
                    Some("setup_default__collect"),
                    &questions_manifest_inline,
                    "../components/questions/questions.wasm",
                )?;

                let apply_payload = merge_payload(
                    provision_apply_example.clone(),
                    json!({
                        "dry_run": "{{ state.input.dry_run }}",
                        "plan": { "actions": [] }
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &setup_default,
                    "setup_default__apply",
                    "apply",
                    apply_payload,
                    Some("out"),
                    None,
                    Some("setup_default__validate"),
                    &provision_manifest_inline,
                    "../components/provision/provision.wasm",
                )?;

                if spec
                    .setup
                    .as_ref()
                    .and_then(|setup| setup.emits_success_message)
                    .unwrap_or(true)
                {
                    let summary_text =
                        format!("{} setup_default complete.", spec.provider.provider_type);
                    let summary_payload = merge_payload(
                        templates_example.clone(),
                        json!({
                            "template": summary_text,
                        }),
                    );
                    flow_add_step(
                        flows_dir,
                        &setup_default,
                        "setup_default__summary",
                        "text",
                        summary_payload,
                        Some("out"),
                        None,
                        Some("setup_default__apply"),
                        &templates_manifest_inline,
                        "../components/templates/templates.wasm",
                    )?;
                    flow_update_routing(
                        &setup_default,
                        "setup_default__apply",
                        "setup_default__summary",
                    )?;
                }

                flow_update_routing(
                    &setup_default,
                    "setup_default__emit_questions",
                    "setup_default__collect",
                )?;
                flow_update_routing(
                    &setup_default,
                    "setup_default__collect",
                    "setup_default__validate",
                )?;
                flow_update_routing(
                    &setup_default,
                    "setup_default__validate",
                    "setup_default__apply",
                )?;
                stamp_generated_header(&setup_default, &generated_meta)?;
            }
            "setup_custom" => {
                let setup_custom = flows_dir.join("setup_custom.ygtc");
                flow_new(&setup_custom, "setup_custom", "job")?;
                let emit_payload = merge_payload(
                    questions_emit_example.clone(),
                    json!({
                        "id": format!("{}-setup_custom", provider_id(spec)),
                        "spec_ref": "assets/setup.yaml"
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &setup_custom,
                    "setup_custom__emit_questions",
                    "emit",
                    emit_payload,
                    Some("out"),
                    None,
                    None,
                    &questions_manifest_inline,
                    "../components/questions/questions.wasm",
                )?;

                let collect_payload = merge_payload(
                    templates_example.clone(),
                    json!({
                        "template": "Collect inputs for setup_custom.",
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &setup_custom,
                    "setup_custom__collect",
                    "text",
                    collect_payload,
                    Some("out"),
                    None,
                    Some("setup_custom__emit_questions"),
                    &templates_manifest_inline,
                    "../components/templates/templates.wasm",
                )?;

                let validate_payload = merge_payload(
                    questions_validate_example.clone(),
                    json!({
                        "answers_json": "{{ state.input.answers_json }}",
                        "spec_json": "{{ node.setup_custom__emit_questions }}"
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &setup_custom,
                    "setup_custom__validate",
                    "validate",
                    validate_payload,
                    Some("out"),
                    None,
                    Some("setup_custom__collect"),
                    &questions_manifest_inline,
                    "../components/questions/questions.wasm",
                )?;

                let apply_payload = merge_payload(
                    provision_apply_example.clone(),
                    json!({
                        "dry_run": "{{ state.input.dry_run }}",
                        "plan": { "actions": [] }
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &setup_custom,
                    "setup_custom__apply",
                    "apply",
                    apply_payload,
                    Some("out"),
                    None,
                    Some("setup_custom__validate"),
                    &provision_manifest_inline,
                    "../components/provision/provision.wasm",
                )?;

                if spec
                    .setup
                    .as_ref()
                    .and_then(|setup| setup.emits_success_message)
                    .unwrap_or(true)
                {
                    let summary_text =
                        format!("{} setup_custom complete.", spec.provider.provider_type);
                    let summary_payload = merge_payload(
                        templates_example.clone(),
                        json!({
                            "template": summary_text,
                        }),
                    );
                    flow_add_step(
                        flows_dir,
                        &setup_custom,
                        "setup_custom__summary",
                        "text",
                        summary_payload,
                        Some("out"),
                        None,
                        Some("setup_custom__apply"),
                        &templates_manifest_inline,
                        "../components/templates/templates.wasm",
                    )?;
                    flow_update_routing(
                        &setup_custom,
                        "setup_custom__apply",
                        "setup_custom__summary",
                    )?;
                }

                flow_update_routing(
                    &setup_custom,
                    "setup_custom__emit_questions",
                    "setup_custom__collect",
                )?;
                flow_update_routing(
                    &setup_custom,
                    "setup_custom__collect",
                    "setup_custom__validate",
                )?;
                flow_update_routing(
                    &setup_custom,
                    "setup_custom__validate",
                    "setup_custom__apply",
                )?;
                stamp_generated_header(&setup_custom, &generated_meta)?;
            }
            "verify_webhooks" => {
                let verify = flows_dir.join("verify_webhooks.ygtc");
                flow_new(&verify, "verify_webhooks", "job")?;
                let payload = merge_payload(
                    templates_example.clone(),
                    json!({
                        "template": "Webhook verification complete.",
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &verify,
                    "summary",
                    "text",
                    payload,
                    Some("out"),
                    None,
                    None,
                    &templates_manifest_inline,
                    "../components/templates/templates.wasm",
                )?;
                stamp_generated_header(&verify, &generated_meta)?;
            }
            "sync_subscriptions" => {
                let sync = flows_dir.join("sync_subscriptions.ygtc");
                flow_new(&sync, "sync_subscriptions", "job")?;
                let payload = merge_payload(
                    templates_example.clone(),
                    json!({
                        "template": "Subscriptions synced.",
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &sync,
                    "summary",
                    "text",
                    payload,
                    Some("out"),
                    None,
                    None,
                    &templates_manifest_inline,
                    "../components/templates/templates.wasm",
                )?;
                stamp_generated_header(&sync, &generated_meta)?;
            }
            "rotate_credentials" => {
                let rotate = flows_dir.join("rotate_credentials.ygtc");
                flow_new(&rotate, "rotate_credentials", "job")?;
                let payload = merge_payload(
                    templates_example.clone(),
                    json!({
                        "template": "Credential rotation complete.",
                    }),
                );
                flow_add_step(
                    flows_dir,
                    &rotate,
                    "summary",
                    "text",
                    payload,
                    Some("out"),
                    None,
                    None,
                    &templates_manifest_inline,
                    "../components/templates/templates.wasm",
                )?;
                stamp_generated_header(&rotate, &generated_meta)?;
            }
            other => {
                return Err(anyhow::anyhow!("unsupported flow type: {}", other));
            }
        }
    }

    Ok(())
}

fn inline_manifest(manifest_path: &Path, out_dir: &Path, name: &str) -> Result<PathBuf> {
    let base_dir = manifest_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("manifest has no parent directory"))?;
    let contents = fs::read_to_string(manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let mut manifest: serde_json::Value =
        serde_json::from_str(&contents).context("parsing manifest json")?;
    if let Some(ops) = manifest
        .get_mut("operations")
        .and_then(|v| v.as_array_mut())
    {
        for op in ops {
            if let Some(obj) = op.as_object_mut() {
                for key in ["input_schema", "output_schema"] {
                    if let Some(schema) = obj.get(key) {
                        let inlined = inline_schema(schema, base_dir)?;
                        obj.insert(key.to_string(), inlined);
                    }
                }
            }
        }
    }
    let out_path = out_dir.join(name);
    let rendered = serde_json::to_string(&manifest)?;
    fs::write(&out_path, rendered).with_context(|| format!("writing {}", out_path.display()))?;
    Ok(out_path)
}

fn write_inline_manifest(src_manifest: &Path, dest_path: &Path) -> Result<()> {
    let base_dir = src_manifest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("manifest has no parent directory"))?;
    let contents = fs::read_to_string(src_manifest)
        .with_context(|| format!("reading {}", src_manifest.display()))?;
    let mut manifest: serde_json::Value =
        serde_json::from_str(&contents).context("parsing manifest json")?;
    if let Some(ops) = manifest
        .get_mut("operations")
        .and_then(|v| v.as_array_mut())
    {
        for op in ops {
            if let Some(obj) = op.as_object_mut() {
                for key in ["input_schema", "output_schema"] {
                    if let Some(schema) = obj.get(key) {
                        let inlined = inline_schema(schema, base_dir)?;
                        obj.insert(key.to_string(), inlined);
                    }
                }
            }
        }
    }
    let rendered = serde_json::to_string(&manifest)?;
    fs::write(dest_path, rendered).with_context(|| format!("writing {}", dest_path.display()))?;
    Ok(())
}

fn inline_schema(schema: &serde_json::Value, base_dir: &Path) -> Result<serde_json::Value> {
    let ref_path = schema.get("$ref").and_then(|v| v.as_str());
    let Some(ref_path) = ref_path else {
        return Ok(schema.clone());
    };
    let file_part = ref_path.split('#').next().unwrap_or("");
    if file_part.is_empty() {
        return Ok(schema.clone());
    }
    let ref_file = base_dir.join(file_part);
    let contents =
        fs::read_to_string(&ref_file).with_context(|| format!("reading {}", ref_file.display()))?;
    let resolved: serde_json::Value =
        serde_json::from_str(&contents).context("parsing schema json")?;
    if resolved.get("$ref").is_some() {
        let next_base = ref_file.parent().unwrap_or(base_dir);
        return inline_schema(&resolved, next_base);
    }
    Ok(resolved)
}

fn diagnostics_summary(spec: &Spec) -> String {
    let (config_keys, secret_keys) = setup_keys(spec);
    if config_keys.is_empty() && secret_keys.is_empty() {
        return "Diagnostics ok; no config/secrets required.".to_string();
    }
    let mut summary = String::from("Diagnostics ok. Expected keys:");
    if !config_keys.is_empty() {
        summary.push_str(&format!(" config [{}]", config_keys.join(", ")));
    }
    if !secret_keys.is_empty() {
        summary.push_str(&format!(" secrets [{}]", secret_keys.join(", ")));
    }
    summary
}

fn requirements_payload(spec: &Spec) -> Result<String> {
    let requirements = spec
        .requirements
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("requirements section missing"))?;
    let payload = json!({
        "required_args": requirements.required_args.clone(),
        "optional_args": requirements.optional_args.clone(),
        "examples": requirements.examples.clone(),
        "notes": requirements.notes.clone().unwrap_or_default(),
    });
    Ok(serde_json::to_string(&payload)?)
}

fn setup_keys(spec: &Spec) -> (Vec<String>, Vec<String>) {
    let mut config_keys = Vec::new();
    let mut secret_keys = Vec::new();
    let Some(setup) = &spec.setup else {
        return (config_keys, secret_keys);
    };
    for question in &setup.questions {
        if let Some(key) = question.write_to.strip_prefix("config:") {
            config_keys.push(key.trim().to_string());
        } else if let Some(key) = question.write_to.strip_prefix("secrets:") {
            secret_keys.push(key.trim().to_string());
        }
    }
    (config_keys, secret_keys)
}

fn flow_new(flow_path: &Path, flow_id: &str, flow_type: &str) -> Result<()> {
    let status = Command::new("greentic-flow")
        .args([
            "new",
            "--flow",
            flow_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid flow path"))?,
            "--id",
            flow_id,
            "--type",
            flow_type,
        ])
        .status()
        .with_context(|| format!("running greentic-flow new on {}", flow_path.display()))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "greentic-flow new failed for {}",
            flow_path.display()
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn flow_add_step(
    flows_dir: &Path,
    flow_path: &Path,
    node_id: &str,
    operation: &str,
    payload: serde_json::Value,
    routing: Option<&str>,
    routing_next: Option<&str>,
    after: Option<&str>,
    manifest_path: &Path,
    local_wasm: &str,
) -> Result<()> {
    let payload_json = serde_json::to_string(&payload)?;
    let flow = flow_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid flow path"))?;
    let manifest = manifest_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid manifest path"))?;
    let mut base_args = vec![
        "add-step",
        "--flow",
        flow,
        "--node-id",
        node_id,
        "--operation",
        operation,
        "--payload",
        &payload_json,
        "--manifest",
        manifest,
        "--local-wasm",
        local_wasm,
    ];
    if let Some(anchor) = after {
        base_args.push("--after");
        base_args.push(anchor);
    }
    match routing {
        Some("out") => {
            base_args.push("--routing-out");
        }
        Some("next") => {
            if let Some(next) = routing_next {
                base_args.push("--routing-next");
                base_args.push(next);
            }
        }
        _ => {}
    }
    let status = Command::new("greentic-flow")
        .current_dir(flows_dir)
        .args(&base_args)
        .status()
        .with_context(|| format!("running greentic-flow add-step on {}", flow_path.display()))?;
    if !status.success() {
        eprintln!(
            "warning: greentic-flow add-step failed for {}:{} (strict); retrying with --permissive",
            flow_path.display(),
            node_id
        );
        let mut permissive_args = base_args;
        permissive_args.insert(1, "--permissive");
        let status = Command::new("greentic-flow")
            .current_dir(flows_dir)
            .args(&permissive_args)
            .status()
            .with_context(|| {
                format!(
                    "running greentic-flow add-step on {} (permissive)",
                    flow_path.display()
                )
            })?;
        if !status.success() {
            return Err(anyhow::anyhow!(
                "greentic-flow add-step failed for {}:{}",
                flow_path.display(),
                node_id
            ));
        }
    }
    Ok(())
}

fn flow_update_routing(flow_path: &Path, step: &str, next: &str) -> Result<()> {
    let flow = flow_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid flow path"))?;
    let args = [
        "update-step",
        "--flow",
        flow,
        "--step",
        step,
        "--routing-next",
        next,
    ];
    let status = Command::new("greentic-flow")
        .args(args)
        .status()
        .with_context(|| {
            format!(
                "running greentic-flow update-step on {}",
                flow_path.display()
            )
        })?;
    if !status.success() {
        eprintln!(
            "warning: greentic-flow update-step failed for {}:{} (strict); retrying with --permissive",
            flow_path.display(),
            step
        );
        let status = Command::new("greentic-flow")
            .args(["update-step", "--permissive"])
            .args(args[1..].iter().copied())
            .status()
            .with_context(|| {
                format!(
                    "running greentic-flow update-step on {} (permissive)",
                    flow_path.display()
                )
            })?;
        if !status.success() {
            return Err(anyhow::anyhow!(
                "greentic-flow update-step failed for {}:{}",
                flow_path.display(),
                step
            ));
        }
    }
    Ok(())
}

fn answers_example(
    manifest_path: &Path,
    operation: &str,
    name: &str,
    out_dir: &Path,
) -> Result<serde_json::Value> {
    let manifest = manifest_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid manifest path"))?;
    let mut cmd = Command::new("greentic-flow");
    cmd.args([
        "answers",
        "--component",
        manifest,
        "--operation",
        operation,
        "--name",
        name,
        "--out-dir",
    ])
    .arg(out_dir);
    let status = cmd
        .status()
        .with_context(|| format!("running greentic-flow answers for {}", name))?;
    if !status.success() {
        eprintln!(
            "warning: greentic-flow answers failed for {} (strict); retrying with --permissive",
            manifest_path.display()
        );
        let mut permissive = Command::new("greentic-flow");
        permissive
            .args([
                "answers",
                "--permissive",
                "--component",
                manifest,
                "--operation",
                operation,
                "--name",
                name,
                "--out-dir",
            ])
            .arg(out_dir);
        let status = permissive
            .status()
            .with_context(|| format!("running greentic-flow answers for {} (permissive)", name))?;
        if !status.success() {
            return Err(anyhow::anyhow!(
                "greentic-flow answers failed for {}",
                manifest_path.display()
            ));
        }
    }
    let example_path = out_dir.join(format!("{}.example.json", name));
    let contents = fs::read_to_string(&example_path)
        .with_context(|| format!("reading {}", example_path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&contents)?;
    Ok(value)
}

fn merge_payload(example: serde_json::Value, overrides: serde_json::Value) -> serde_json::Value {
    let mut merged = example;
    merge_values(&mut merged, overrides);
    merged
}

fn merge_values(base: &mut serde_json::Value, overrides: serde_json::Value) {
    match (base, overrides) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(over_map)) => {
            for (k, v) in over_map {
                match base_map.entry(k) {
                    serde_json::map::Entry::Vacant(entry) => {
                        entry.insert(v);
                    }
                    serde_json::map::Entry::Occupied(mut entry) => {
                        merge_values(entry.get_mut(), v);
                    }
                }
            }
        }
        (base_slot, override_value) => {
            *base_slot = override_value;
        }
    }
}

fn base_templates_payload(provider: &str) -> serde_json::Value {
    json!({
        "msg": {
            "provider": provider,
            "id": "{{ state.input.msg.id }}",
            "tenant_id": "{{ state.input.msg.tenant_id }}",
            "channel": "{{ state.input.msg.channel }}",
            "session_id": "{{ state.input.msg.session_id }}",
            "reply_scope": "{{ state.input.msg.reply_scope }}",
            "user_id": "{{ state.input.msg.user_id }}",
            "correlation_id": "{{ state.input.msg.correlation_id }}",
            "text": "{{ state.input.msg.text }}",
            "metadata": "{{ state.input.msg.metadata }}",
            "message": {
                "id": "{{ state.input.msg.message.id }}",
                "text": "{{ state.input.msg.message.text }}"
            }
        },
        "output_path": "text",
        "wrap": true
    })
}

struct GeneratedFlowMetadata {
    spec_path: String,
    spec_hash: String,
    manifest_hash: String,
    schemas_hash: String,
}

fn generated_flow_metadata(
    spec_path: &Path,
    manifest_paths: &[PathBuf],
) -> Result<GeneratedFlowMetadata> {
    let spec_hash = format!("sha256:{}", hash_files(&[spec_path.to_path_buf()])?);
    let manifest_hash = format!("sha256:{}", hash_files(manifest_paths)?);
    let schema_paths = collect_schema_files(manifest_paths)?;
    let schemas_hash = if schema_paths.is_empty() {
        "sha256:".to_string()
    } else {
        format!("sha256:{}", hash_files(&schema_paths)?)
    };
    Ok(GeneratedFlowMetadata {
        spec_path: spec_path.display().to_string(),
        spec_hash,
        manifest_hash,
        schemas_hash,
    })
}

fn collect_schema_files(manifest_paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for manifest in manifest_paths {
        if let Some(parent) = manifest.parent() {
            let schemas_dir = parent.join("schemas");
            if schemas_dir.is_dir() {
                for entry in walk_dir_files(&schemas_dir)? {
                    files.push(entry);
                }
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn walk_dir_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk_dir_files(&path)?);
        } else {
            files.push(path);
        }
    }
    Ok(files)
}

fn hash_files(paths: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut sorted = paths.to_vec();
    sorted.sort();
    for path in sorted {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update([0u8]);
        let data = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        hasher.update(&data);
        hasher.update([0u8]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn stamp_generated_header(flow_path: &Path, meta: &GeneratedFlowMetadata) -> Result<()> {
    let contents = fs::read_to_string(flow_path)
        .with_context(|| format!("reading {}", flow_path.display()))?;
    let mut flow: serde_yaml::Value =
        serde_yaml::from_str(&contents).context("parsing flow yaml")?;
    let mapping = flow
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("flow yaml is not a mapping"))?;

    mapping.insert(
        serde_yaml::Value::String("x-generated-by".to_string()),
        serde_yaml::Value::String("greentic-messaging-packgen".to_string()),
    );
    let mut source = serde_yaml::Mapping::new();
    source.insert(
        serde_yaml::Value::String("spec".to_string()),
        serde_yaml::Value::String(meta.spec_path.clone()),
    );
    source.insert(
        serde_yaml::Value::String("spec-hash".to_string()),
        serde_yaml::Value::String(meta.spec_hash.clone()),
    );
    source.insert(
        serde_yaml::Value::String("manifest-hash".to_string()),
        serde_yaml::Value::String(meta.manifest_hash.clone()),
    );
    source.insert(
        serde_yaml::Value::String("schemas-hash".to_string()),
        serde_yaml::Value::String(meta.schemas_hash.clone()),
    );
    mapping.insert(
        serde_yaml::Value::String("x-generated-from".to_string()),
        serde_yaml::Value::Mapping(source),
    );

    let updated = serde_yaml::to_string(&flow)?;
    fs::write(flow_path, updated).with_context(|| format!("writing {}", flow_path.display()))?;
    Ok(())
}

fn stage_components(
    components_dir: &Path,
    spec_path: &Path,
    spec: &Spec,
    flows: &[String],
) -> Result<()> {
    let root = workspace_root()?;
    let provision_src = root.join("components/provision/provision.wasm");
    let provision_manifest = root.join("components/provision/component.manifest.json");
    let questions_src = root.join("components/questions/questions.wasm");
    let questions_manifest = root.join("components/questions/component.manifest.json");
    let templates_src = root.join("components/templates/templates.wasm");
    let templates_manifest = root.join("components/templates/component.manifest.json");

    let mut needs_templates = false;
    let mut needs_questions = false;
    let mut needs_provision = false;
    for flow in flows {
        match flow.as_str() {
            "setup_default" => {
                needs_templates = true;
                needs_questions = true;
                needs_provision = true;
            }
            "diagnostics" | "requirements" => {
                needs_templates = true;
            }
            _ => {}
        }
    }

    if needs_provision {
        let provision_dir = components_dir.join("provision");
        copy_file(&provision_src, &provision_dir.join("provision.wasm"))?;
        write_inline_manifest(
            &provision_manifest,
            &provision_dir.join("component.manifest.json"),
        )?;
        copy_dir_if_exists(
            &root.join("components/provision/schemas"),
            &provision_dir.join("schemas"),
        )?;
    }
    if needs_questions {
        let questions_dir = components_dir.join("questions");
        copy_file(&questions_src, &questions_dir.join("questions.wasm"))?;
        write_inline_manifest(
            &questions_manifest,
            &questions_dir.join("component.manifest.json"),
        )?;
        copy_dir_if_exists(
            &root.join("components/questions/schemas"),
            &questions_dir.join("schemas"),
        )?;
    }
    if needs_templates {
        let templates_dir = components_dir.join("templates");
        copy_file(&templates_src, &templates_dir.join("templates.wasm"))?;
        write_inline_manifest(
            &templates_manifest,
            &templates_dir.join("component.manifest.json"),
        )?;
        copy_dir_if_exists(
            &root.join("components/templates/schemas"),
            &templates_dir.join("schemas"),
        )?;
    }

    stage_component(components_dir, spec_path, &spec.components.adapter)?;
    if let Some(renderer) = &spec.components.renderer {
        stage_component(components_dir, spec_path, renderer)?;
    }

    Ok(())
}

fn stage_component(
    components_dir: &Path,
    spec_path: &Path,
    component: &ComponentSpec,
) -> Result<()> {
    let spec_dir = spec_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("spec has no parent directory"))?;
    let component_id = component_id(component, Some(spec_dir))?;
    let dest_dir = components_dir.join(&component_id);

    let component_ref = component.component_ref.as_str();
    if let Some(local_path) = component_ref.strip_prefix("local:") {
        let src_path = resolve_local_path(spec_dir, local_path)?;
        copy_file(&src_path, &dest_dir.join(format!("{component_id}.wasm")))?;
        let manifest_path = component
            .manifest
            .clone()
            .map(|path| resolve_local_path(spec_dir, path));
        let manifest_path = match manifest_path {
            Some(Ok(path)) => Some(path),
            Some(Err(err)) => return Err(err),
            None => infer_manifest_path(&src_path),
        };
        if let Some(manifest_path) = manifest_path
            && manifest_has_id(&manifest_path)?
        {
            write_inline_manifest(&manifest_path, &dest_dir.join("component.manifest.json"))?;
            if let Some(src_dir) = manifest_path.parent() {
                copy_dir_if_exists(&src_dir.join("schemas"), &dest_dir.join("schemas"))?;
            }
        }
    } else if component_ref.starts_with("oci://") {
        fetch_component(
            component_ref,
            &dest_dir.join(format!("{component_id}.wasm")),
        )?;
        if let Some(manifest_path) = &component.manifest {
            let resolved = resolve_local_path(spec_dir, manifest_path)?;
            if manifest_has_id(&resolved)? {
                write_inline_manifest(&resolved, &dest_dir.join("component.manifest.json"))?;
                if let Some(src_dir) = resolved.parent() {
                    copy_dir_if_exists(&src_dir.join("schemas"), &dest_dir.join("schemas"))?;
                }
            }
        }
    } else {
        return Err(anyhow::anyhow!(
            "unsupported component_ref scheme: {}",
            component_ref
        ));
    }

    Ok(())
}

fn component_id(component: &ComponentSpec, spec_dir: Option<&Path>) -> Result<String> {
    if let Some(id) = &component.id {
        return Ok(id.clone());
    }
    let component_ref = component.component_ref.as_str();
    if let Some(local_path) = component_ref.strip_prefix("local:") {
        let path = resolve_local_path(spec_dir.unwrap_or_else(|| Path::new(".")), local_path)?;
        let stem = path
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid local component path: {}", path.display()))?;
        return Ok(stem.to_string());
    }
    Err(anyhow::anyhow!(
        "components.adapter.id must be set for non-local component refs"
    ))
}

fn infer_manifest_path(wasm_path: &Path) -> Option<PathBuf> {
    let parent = wasm_path.parent()?;
    let manifest = parent.join("component.manifest.json");
    if manifest.exists() {
        Some(manifest)
    } else {
        None
    }
}

fn fetch_component(component_ref: &str, out_path: &Path) -> Result<()> {
    let status = Command::new("greentic-component")
        .args(["store", "fetch", component_ref, "--out"])
        .arg(out_path)
        .status()
        .with_context(|| format!("fetching component {}", component_ref))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "greentic-component store fetch failed for {}",
            component_ref
        ));
    }
    Ok(())
}

fn resolve_local_path(spec_dir: &Path, path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        spec_dir.join(path)
    };
    Ok(resolved)
}

fn stage_schema(dest_dir: &Path, schema_root: &Path, spec_path: &Path, spec: &Spec) -> Result<()> {
    let root = workspace_root()?;
    let ref_path = config_schema_ref(spec);
    let schema_src = if ref_path.starts_with("schemas/") {
        root.join(ref_path)
    } else {
        resolve_local_path(
            spec_path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("spec has no parent directory"))?,
            ref_path,
        )?
    };
    copy_file(&schema_src, &dest_dir.join("public.config.schema.json"))?;
    let provider_dir = schema_root.join(&spec.provider.provider_type);
    copy_file(&schema_src, &provider_dir.join("config.schema.json"))?;
    Ok(())
}

fn write_empty_secret_requirements(out_dir: &Path) -> Result<()> {
    let contents = "[]\n";
    fs::write(out_dir.join("secret-requirements.json"), contents)
        .context("writing secret-requirements.json")?;
    fs::write(out_dir.join(".secret_requirements.json"), contents)
        .context("writing .secret_requirements.json")?;
    Ok(())
}

fn copy_file(src: &Path, dest: &Path) -> Result<()> {
    if !src.exists() {
        return Err(anyhow::anyhow!("missing required file: {}", src.display()));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, dest).with_context(|| format!("copy {} to {}", src.display(), dest.display()))?;
    Ok(())
}

fn copy_dir_if_exists(src: &Path, dest: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    if !src.is_dir() {
        return Err(anyhow::anyhow!("expected directory: {}", src.display()));
    }
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir_if_exists(&path, &target)?;
        } else {
            fs::copy(&path, &target)
                .with_context(|| format!("copy {} to {}", path.display(), target.display()))?;
        }
    }
    Ok(())
}

fn manifest_has_id(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let contents =
        fs::read_to_string(path).with_context(|| format!("reading manifest {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&contents)?;
    Ok(value.get("id").and_then(|v| v.as_str()).is_some())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir()?;
    Ok(cwd.join(path))
}

fn workspace_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("Cargo.toml").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err(anyhow::anyhow!("could not find workspace root"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> Spec {
        let world = known_worlds_from_interfaces()
            .expect("known worlds")
            .into_iter()
            .next()
            .expect("world");
        let tier = known_render_tiers_from_interfaces()
            .expect("render tiers")
            .into_iter()
            .next()
            .expect("render tier");
        Spec {
            pack: PackSpec {
                id: "messaging-test".to_string(),
                name: Some("Messaging Test".to_string()),
                version: "0.1.0".to_string(),
                publisher: Some("Greentic".to_string()),
                kind: Some("application".to_string()),
            },
            provider: ProviderSpec {
                provider_type: "messaging.test".to_string(),
                provider_id: Some("test".to_string()),
                ops: vec!["send".to_string()],
                capabilities: json!({ "render_tiers": [tier] }),
                config_schema_ref: None,
                state_schema_ref: None,
                docs_ref: None,
            },
            components: ComponentsSpec {
                adapter: ComponentSpec {
                    component_ref: "local:components/adapter.wasm".to_string(),
                    world,
                    export: Some("schema-core-api".to_string()),
                    id: None,
                    manifest: None,
                },
                renderer: None,
                ingress: None,
                subscriptions: None,
            },
            flows: FlowsSpec {
                generate: vec!["setup_default".to_string(), "requirements".to_string()],
                include: Vec::new(),
            },
            setup: Some(SetupSpec {
                questions: Vec::new(),
                emits_success_message: Some(false),
                asset_path: None,
            }),
            requirements: Some(RequirementsSpec {
                required_args: vec!["text".to_string()],
                optional_args: Vec::new(),
                examples: Vec::new(),
                notes: None,
            }),
            validators: None,
            source: None,
            contract: None,
        }
    }

    #[test]
    fn validate_spec_rejects_unknown_world() {
        let mut spec = sample_spec();
        spec.components.adapter.world = "unknown:world@0.1.0".to_string();
        let err = validate_spec(Path::new("spec.yaml"), &spec).expect_err("expected error");
        assert!(
            err.to_string().contains("not a known WIT world"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_spec_rejects_unknown_op() {
        let mut spec = sample_spec();
        spec.provider.ops = vec!["bogus".to_string()];
        let err = validate_spec(Path::new("spec.yaml"), &spec).expect_err("expected error");
        assert!(
            err.to_string().contains("unknown op"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_spec_rejects_unknown_flow() {
        let mut spec = sample_spec();
        spec.flows.generate.push("not-a-flow".to_string());
        let err = validate_spec(Path::new("spec.yaml"), &spec).expect_err("expected error");
        assert!(
            err.to_string().contains("unknown flow"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_spec_rejects_missing_pack_id() {
        let mut spec = sample_spec();
        spec.pack.id = " ".to_string();
        let err = validate_spec(Path::new("spec.yaml"), &spec).expect_err("expected error");
        assert!(
            err.to_string().contains("spec.pack.id must be set"),
            "unexpected error: {err}"
        );
    }
}
