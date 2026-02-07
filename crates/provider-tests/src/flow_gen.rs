use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum StepRouting {
    Out,
    Next(String),
}

#[derive(Debug, Clone)]
pub struct StepSpec {
    pub node_id: String,
    pub operation: String,
    pub payload: Value,
    pub manifest_path: PathBuf,
    pub local_wasm: String,
    pub routing: Option<StepRouting>,
    pub after: Option<String>,
}

pub fn generate_flow_via_cli(pack_root: &Path, flow_name: &str, steps: &[StepSpec]) -> Result<()> {
    let flows_dir = pack_root.join("flows");
    std::fs::create_dir_all(&flows_dir)?;
    let flow_path = flows_dir.join(format!("{flow_name}.ygtc"));

    run_flow_cmd(
        flow_cmd()?.args([
            "new",
            "--force",
            "--flow",
            flow_path
                .to_str()
                .ok_or_else(|| anyhow!("invalid flow path"))?,
            "--id",
            flow_name,
            "--type",
            "job",
        ]),
        "greentic-flow new",
    )?;

    for step in steps {
        let payload_json = serde_json::to_string(&step.payload)?;
        let mut cmd = flow_cmd()?;
        cmd.current_dir(&flows_dir);
        cmd.args([
            "add-step",
            "--flow",
            flow_path
                .to_str()
                .ok_or_else(|| anyhow!("invalid flow path"))?,
            "--node-id",
            &step.node_id,
            "--operation",
            &step.operation,
            "--payload",
            &payload_json,
            "--manifest",
            step.manifest_path
                .to_str()
                .ok_or_else(|| anyhow!("invalid manifest path"))?,
            "--local-wasm",
            &step.local_wasm,
        ]);

        if let Some(after) = &step.after {
            cmd.args(["--after", after]);
        }

        match &step.routing {
            Some(StepRouting::Out) => {
                cmd.arg("--routing-out");
            }
            Some(StepRouting::Next(next)) => {
                cmd.args(["--routing-next", next]);
            }
            None => {}
        }

        run_flow_cmd(&mut cmd, "greentic-flow add-step")?;
    }

    Ok(())
}

fn flow_cmd() -> Result<Command> {
    if let Some(bin) = env::var_os("GREENTIC_FLOW_BIN") {
        return Ok(Command::new(bin));
    }

    if let Some(path) = find_in_path("greentic-flow") {
        return Ok(Command::new(path));
    }

    let mut cmd = Command::new("cargo");
    cmd.args(["run", "--quiet", "--bin", "greentic-flow", "--"]);
    Ok(cmd)
}

fn run_flow_cmd(cmd: &mut Command, label: &str) -> Result<()> {
    let output = cmd.output().with_context(|| format!("running {label}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(anyhow!(
        "{label} failed (exit={:?})\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout,
        stderr
    ))
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    env::split_paths(&path_var).find_map(|dir| find_executable_in_dir(name, &dir))
}

fn find_executable_in_dir(name: &str, dir: &Path) -> Option<PathBuf> {
    let candidate = dir.join(name);
    if candidate.exists() {
        return Some(candidate);
    }
    if cfg!(windows) {
        let exe = dir.join(format!("{name}.exe"));
        if exe.exists() {
            return Some(exe);
        }
    }
    None
}
