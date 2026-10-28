use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
};
use clap::{Parser, Subcommand};
use futures_util::stream::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const CONTAINER_WORKSPACE: &str = "/workspace";

// serde_yaml will look for a top level "jobs:" key
// automatically matching the field name jobs below
#[derive(Debug, Deserialize)]
struct Workflow {
    #[serde(default)]
    env: HashMap<String, serde_yaml::Value>,
    jobs: HashMap<String, Job>,
}

// what machine it runs on and the ordered list of steps
#[derive(Debug, Deserialize)]
struct Job {
    #[serde(default)]
    env: HashMap<String, serde_yaml::Value>,
    // yaml uses "runs-on" but hyphens arent allowed in
    // rust identifiers so rename explicitly
    #[serde(rename = "runs-on")]
    runs_on: String,
    steps: Vec<Step>,
    strategy: Option<Strategy>,
}

// one step. name and env are optional since not every step has them
#[derive(Debug, Deserialize)]
struct Step {
    name: Option<String>,
    run: Option<String>,
    #[serde(default)]
    env: HashMap<String, serde_yaml::Value>,
}

#[derive(Parser)]
#[command(name = "preflight-ci")]
#[command(about = "Run your CI workflow before you push")]
struct Cli {
    // tells clap the actual command lives inside commands enum
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // run a job from a workflow file
    Run {
        // no attribute = positional argument
        job: String,

        // optional: if ommitted, search .github
        #[arg(long)]
        workflow: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct Strategy {
    matrix: HashMap<String, Vec<serde_yaml::Value>>,
}

fn value_to_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        _ => format!("{:?}", value),
    }
}

fn env_to_string(env: &HashMap<String, serde_yaml::Value>) -> HashMap<String, String> {
    env.iter()
        .map(|(k, v)| (k.clone(), value_to_string(v)))
        .collect()
}

fn matrix_combinations(
    matrix: &HashMap<String, Vec<serde_yaml::Value>>,
) -> Vec<HashMap<String, String>> {
    let mut combinations: Vec<HashMap<String, String>> = vec![HashMap::new()];

    for (key, values) in matrix {
        let mut next_combinations = Vec::new();
        for combo in &combinations {
            for value in values {
                let mut new_combo = combo.clone();
                new_combo.insert(key.clone(), value_to_string(value));
                next_combinations.push(new_combo);
            }
        }
        combinations = next_combinations;
    }
    combinations
}

fn substitute_matrix_vars(text: &str, combo: &HashMap<String, String>) -> String {
    let mut result = text.to_string();
    for (key, value) in combo {
        let placeholder = format!("${{{{ matrix.{} }}}}", key);
        result = result.replace(&placeholder, value);
    }
    result
}

fn map_runs_on_to_image(runs_on: &str) -> anyhow::Result<&str> {
    match runs_on {
        "ubuntu-latest" | "ubuntu:24.04" => Ok("ubuntu:24.04"),
        "ubuntu-22.04" => Ok("ubuntu:22.04"),
        "ubuntu-20.04" => Ok("ubuntu:20.04"),
        "ubuntu-26.04" => Ok("ubuntu:26.04"),

        other if other.starts_with("macos") => anyhow::bail!(
            "'{}' targets macOS, which preflight-ci can't run locally - Docker containers are Linux only",
            other
        ),
        other if other.starts_with("windows") => anyhow::bail!(
            "'{}' targets Window, which preflight-ci can't run locally - Docker containers are Linux only",
            other
        ),
        other => {
            println!(
                "warning: no image mapping for '{}', defaulting to ubuntu:22.04",
                other
            );
            Ok("ubuntu:22.04")
        }
    }
}

async fn pull_image(docker: &Docker, image: &str) -> anyhow::Result<()> {
    println!("Pulling image: {}", image);
    let pull_options = CreateImageOptionsBuilder::new().from_image(image).build();

    let pb = ProgressBar::new(0);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {bytes} / {total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("=>-"),
    );

    let mut layer_progress: HashMap<String, (u64, u64)> = HashMap::new();
    let mut pull_stream = docker.create_image(Some(pull_options), None, None);
    while let Some(result) = pull_stream.next().await {
        let info = result?;
        #[allow(clippy::collapsible_if)]
        if let (Some(id), Some(detail)) = (info.id, info.progress_detail) {
            if let (Some(current), Some(total)) = (detail.current, detail.total) {
                layer_progress.insert(id, (current as u64, total as u64));
                let total_current: u64 = layer_progress.values().map(|(c, _)| *c).sum();
                let total_total: u64 = layer_progress.values().map(|(_, t)| *t).sum();
                pb.set_length(total_total);
                pb.set_position(total_current);
            }
        }
    }
    pb.finish_with_message("done");
    println!("Image ready. \n");

    Ok(())
}

async fn run_step(
    docker: &Docker,
    container_name: &str,
    step: &Step,
    base_env: &HashMap<String, String>,
    combo: &HashMap<String, String>,
) -> anyhow::Result<bool> {
    let Some(run_command_raw) = &step.run else {
        println!("Skipping step (no 'run' command - 'uses' isn't supported yet)\n");
        return Ok(true);
    };
    let run_command = substitute_matrix_vars(run_command_raw, combo);

    let step_name = step.name.as_deref().unwrap_or("(unnamed step)");
    println!("--- {} ---", step_name);

    let mut merged_env = base_env.clone();
    merged_env.extend(env_to_string(&step.env));

    let env_vars: Vec<String> = merged_env
        .iter()
        .map(|(key, value)| format!("{}={}", key, substitute_matrix_vars(value, combo)))
        .collect();
    let env_refs: Vec<&str> = env_vars.iter().map(String::as_str).collect();

    let exec = docker
        .create_exec(
            container_name,
            CreateExecOptions {
                cmd: Some(vec!["sh", "-c", run_command.as_str()]),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                env: Some(env_refs),
                working_dir: Some(CONTAINER_WORKSPACE),
                ..Default::default()
            },
        )
        .await?;

    if let StartExecResults::Attached { mut output, .. } = docker.start_exec(&exec.id, None).await?
    {
        while let Some(Ok(chunk)) = output.next().await {
            print!("{}", chunk);
        }
    }

    let inspect = docker.inspect_exec(&exec.id).await?;
    let exit_code = inspect.exit_code.unwrap_or(-1);

    if exit_code != 0 {
        println!("\nStep failed with exit code {}\n", exit_code);
        return Ok(false);
    }
    println!();
    Ok(true)
}

async fn run_steps_in_container(
    docker: &Docker,
    container_name: &str,
    job_def: &Job,
    base_env: &HashMap<String, String>,
    combo: &HashMap<String, String>,
) -> anyhow::Result<bool> {
    docker.start_container(container_name, None).await?;

    for step in &job_def.steps {
        if !run_step(docker, container_name, step, base_env, combo).await? {
            return Ok(false);
        }
    }
    Ok(true)
}
async fn run_combination(
    docker: &Docker,
    image: &str,
    job_def: &Job,
    base_env: &HashMap<String, String>,
    combo: &HashMap<String, String>,
) -> anyhow::Result<bool> {
    if !combo.is_empty() {
        let combo_desc: Vec<String> = combo.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        println!("### Matrix: {} ###\n", combo_desc.join(", "));
    }

    let unique_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let container_name = format!("preflight-ci-{}", unique_suffix);

    let host_path = std::env::current_dir()?.display().to_string();
    let host_config = HostConfig {
        binds: Some(vec![format!("{}:{}", host_path, CONTAINER_WORKSPACE)]),
        ..Default::default()
    };

    let config = ContainerCreateBody {
        image: Some(image.to_string()),
        cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
        working_dir: Some(CONTAINER_WORKSPACE.to_string()),
        host_config: Some(host_config),
        ..Default::default()
    };
    let create_options = CreateContainerOptionsBuilder::new()
        .name(&container_name)
        .build();

    docker
        .create_container(Some(create_options), config)
        .await?;
    println!("Running in container using image: {}\n", image);

    let result = run_steps_in_container(docker, &container_name, job_def, base_env, combo).await;

    let remove_options = RemoveContainerOptionsBuilder::new().force(true).build();
    let cleanup = docker
        .remove_container(&container_name, Some(remove_options))
        .await;

    let passed = result?;
    cleanup?;
    Ok(passed)
}

fn load_workflow(path: &str) -> anyhow::Result<Workflow> {
    let raw = fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&raw)?)
}

fn find_workflow_for_job(job: &str) -> anyhow::Result<String> {
    let dir = ".github/workflows";
    let entries = fs::read_dir(dir).map_err(|e| {
        anyhow::anyhow!(
            "couldn't read '{}': {} (use --workflow to give a path)",
            dir,
            e
        )
    })?;

    let mut found: Vec<String> = Vec::new();
    for entry in entries {
        let path = entry?.path();

        let is_yaml = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yml") | Some("yaml")
        );
        if !is_yaml {
            continue;
        }

        let path_str = path.display().to_string();
        match load_workflow(&path_str) {
            Ok(wf) => {
                if wf.jobs.contains_key(job) {
                    found.push(path_str);
                }
            }
            Err(e) => println!("warning: skipping '{}': {}", path_str, e),
        }
    }

    // read_dir order is arbitrary so sort for stable output
    found.sort();

    match found.len() {
        0 => anyhow::bail!("no workflow in '{}' defines a job named '{}'", dir, job),
        1 => Ok(found.remove(0)),
        _ => anyhow::bail!(
            "job '{}' is defined in several files:\n {}\nuse --workflow to pick one",
            job,
            found.join("\n ")
        ),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // cli::parse() reads the arguements the user typed
    // and matches them against the shape above
    let cli = Cli::parse();
    let Commands::Run { job, workflow } = cli.command;

    let workflow_path = match workflow {
        Some(path) => path,
        None => find_workflow_for_job(&job)?,
    };
    println!("Using workflow: {}\n", workflow_path);
    let parsed = load_workflow(&workflow_path)?;

    let job_def = parsed
        .jobs
        .get(&job)
        .ok_or_else(|| anyhow::anyhow!("job '{}' not found in workflow", job))?;

    let mut base_env = env_to_string(&parsed.env);
    base_env.extend(env_to_string(&job_def.env));

    let docker = Docker::connect_with_local_defaults()?;
    let image = map_runs_on_to_image(&job_def.runs_on)?;

    pull_image(&docker, image).await?;

    let combinations: Vec<HashMap<String, String>> = match &job_def.strategy {
        Some(strategy) => matrix_combinations(&strategy.matrix),
        None => vec![HashMap::new()],
    };

    let mut overall_passed = true;
    for combo in &combinations {
        if !run_combination(&docker, image, job_def, &base_env, combo).await? {
            overall_passed = false;
        }
    }

    if overall_passed {
        println!("All steps passed");
    } else {
        println!("Build failed");
        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ubuntu_latest_maps_to_know_image() {
        let result = map_runs_on_to_image("ubuntu-latest").unwrap();
        assert_eq!(result, "ubuntu:24.04");
    }

    #[test]
    fn macos_is_rejected() {
        let result = map_runs_on_to_image("macos-latest");
        assert!(result.is_err());
    }

    #[test]
    fn windows_is_rejected() {
        let result = map_runs_on_to_image("windows-latest");
        assert!(result.is_err());
    }

    #[test]
    fn unknown_runner_falls_back_with_warning() {
        let result = map_runs_on_to_image("some-future-runner");
        assert!(result.is_ok());
    }

    #[test]
    fn single_dimension_matrix_produces_one_combo_per_value() {
        let mut matrix = HashMap::new();
        matrix.insert(
            "greeting".to_string(),
            vec![
                serde_yaml::Value::String("hello".to_string()),
                serde_yaml::Value::String("hi".to_string()),
            ],
        );
        let combos = matrix_combinations(&matrix);
        assert_eq!(combos.len(), 2);
    }

    #[test]
    fn two_dimension_matrix_produces_cartesian_product() {
        let mut matrix = HashMap::new();
        matrix.insert(
            "os".to_string(),
            vec![serde_yaml::Value::String("ubuntu".to_string())],
        );
        matrix.insert(
            "version".to_string(),
            vec![
                serde_yaml::Value::Number(18.into()),
                serde_yaml::Value::Number(20.into()),
            ],
        );
        let combos = matrix_combinations(&matrix);
        assert_eq!(combos.len(), 2);
    }

    #[test]
    fn substitution_leaves_text_without_placeholders_unchanged() {
        let combo = HashMap::new();
        let result = substitute_matrix_vars("echo hello", &combo);
        assert_eq!(result, "echo hello");
    }

    #[test]
    fn parses_minimal_workflow() {
        let yaml = r#"
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - name: A step
        run: echo hi
"#;
        let workflow: Workflow = serde_yaml::from_str(yaml).unwrap();
        let job = workflow.jobs.get("test").unwrap();
        assert_eq!(job.runs_on, "ubuntu-latest");
        assert_eq!(job.steps.len(), 1);
    }

    #[test]
    fn env_values_of_any_scalar_type_become_strings() {
        let mut env = HashMap::new();
        env.insert("FLAG".to_string(), serde_yaml::Value::Bool(true));
        env.insert("RETRIES".to_string(), serde_yaml::Value::Number(3.into()));
        let result = env_to_string(&env);
        assert_eq!(result["FLAG"], "true");
        assert_eq!(result["RETRIES"], "3");
    }

    #[test]
    fn later_env_layers_override_earlier_ones() {
        let mut base = HashMap::new();
        base.insert("A".to_string(), "workflow".to_string());
        let mut step = HashMap::new();
        step.insert(
            "A".to_string(),
            serde_yaml::Value::String("step".to_string()),
        );
        base.extend(env_to_string(&step));
        assert_eq!(base["A"], "step");
    }
}
