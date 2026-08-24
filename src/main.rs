use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::CreateImageOptionsBuilder;
use bollard::query_parameters::{CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder};
use clap::{Parser, Subcommand};
use futures_util::stream::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

// serde_yaml will look for a top level "jobs:" key
// automatically matching the field name jobs below
#[derive(Debug, Deserialize)]
struct Workflow {
    jobs: HashMap<String, Job>,
}

// what machine it runs on and the ordered list of steps
#[derive(Debug, Deserialize)]
struct Job {
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
    env: HashMap<String, String>,
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

        // default_value lets it fall back if omitted
        #[arg(long, default_value = "sample-workflow.yml")]
        workflow: String,
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
        "ubuntu-20.04" => Ok("ubuntu-20.04"),
        "ubuntu-26.04" => Ok("ubuntu-26.04"),

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // cli::parse() reads the arguements the user typed
    // and matches them against the shape above
    let cli = Cli::parse();
    let Commands::Run { job, workflow } = cli.command;

    let raw = fs::read_to_string(&workflow)?;
    let parsed: Workflow = serde_yaml::from_str(&raw)?;

    let job_def = parsed
        .jobs
        .get(&job)
        .ok_or_else(|| anyhow::anyhow!("job '{}' not found in workflow", job))?;

    let docker = Docker::connect_with_local_defaults()?;
    let image = map_runs_on_to_image(&job_def.runs_on)?;

    println!("Pulling Image {}...", image);
    let pull_options = CreateImageOptionsBuilder::new().from_image(image).build();

    let pb = ProgressBar::new(0);

    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("=>-"),
    );

    let mut layer_progress: HashMap<String, (u64, u64)> = HashMap::new();

    let mut pull_stream = docker.create_image(Some(pull_options), None, None);
    while let Some(result) = pull_stream.next().await {
        let info = result?;

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
    println!("Image ready. \n");

    let combinations: Vec<HashMap<String, String>> = match &job_def.strategy {
        Some(strategy) => matrix_combinations(&strategy.matrix),
        None => vec![HashMap::new()],
    };

    let mut overall_passed = true;

    for combo in &combinations {
        if !combo.is_empty() {
            let combo_desc: Vec<String> =
                combo.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
            println!("### Matrix: {} ###\n", combo_desc.join(", "));
        }

        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let container_name = format!("preflight-ci-{}", unique_suffix);

        let config = ContainerCreateBody {
            image: Some(image.to_string()),
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            ..Default::default()
        };
        let create_options = CreateContainerOptionsBuilder::new()
            .name(&container_name)
            .build();

        docker
            .create_container(Some(create_options), config)
            .await?;
        docker.start_container(&container_name, None).await?;
        println!("Container started using image: {}\n", image);

        let mut combo_passed = true;

        for step in &job_def.steps {
            // dont support 'uses:' only support 'run:' for the moment so skip 'uses:' without crashing
            let Some(run_command_raw) = &step.run else {
                println!("Skipping step (no 'run' command - 'uses' isnt supported yet \n");
                continue;
            };
            let run_command = substitute_matrix_vars(run_command_raw, combo);

            let step_name = step.name.as_deref().unwrap_or("(unnamed step)");
            println!("=== {} ===", step_name);

            // converts the steps hash,ap into dockers exec apis prefered layout
            let env_vars: Vec<String> = step
                .env
                .iter()
                .map(|(key, value)| format!("{}={}", key, value))
                .collect();

            let env_refs: Vec<&str> = env_vars.iter().map(String::as_str).collect();

            let exec = docker
                .create_exec(
                    &container_name,
                    CreateExecOptions {
                        cmd: Some(vec!["sh", "-c", run_command.as_str()]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        env: Some(env_refs),
                        ..Default::default()
                    },
                )
                .await?;

            if let StartExecResults::Attached { mut output, .. } =
                docker.start_exec(&exec.id, None).await?
            {
                while let Some(Ok(chunk)) = output.next().await {
                    print!("{}", chunk);
                }
            }

            // asks docker what actually happened rather than just showing output
            let inspect = docker.inspect_exec(&exec.id).await?;
            let exit_code = inspect.exit_code.unwrap_or(-1);

            if exit_code != 0 {
                println!("\nStep failed with exit code {}\n", exit_code);
                combo_passed = false;
                break;
            }
            println!();
        }

        let remove_options = RemoveContainerOptionsBuilder::new().force(true).build();
        docker
            .remove_container(&container_name, Some(remove_options))
            .await?;

        if !combo_passed {
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
