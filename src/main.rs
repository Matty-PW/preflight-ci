use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder};
use bollard::Docker;
use futures_util::stream::StreamExt;
use clap::{Parser, Subcommand};
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

fn map_runs_on_to_image(runs_on: &str) -> &str {
    match runs_on {
        "ubuntu-latest" | "ubuntu:22.04" => "ubuntu:22.04",
        other => {
            println!(
                "warning: no image mapping for '{}', defaulting to ubuntu:22.04",
            other
        );
        "ubuntu:22.04"
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
    let image = map_runs_on_to_image(&job_def.runs_on);

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

    docker.create_container(Some(create_options), config).await?;
    docker.start_container(&container_name, None).await?;
    println!("Container started using image: {}\n", image);

    let mut all_passed = true;

    for step in &job_def.steps {
        // dont support 'uses:' only support 'run:' for the moment so skip 'uses:' without crashing
        let Some(run_command) = &step.run else {
            println!("Skipping step (no 'run' command - 'uses' isnt supported yet \n");
            continue;
        };

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
            all_passed = false;
            break;
        }
        println!();

    };

    let remove_options = RemoveContainerOptionsBuilder::new().force(true).build();
    docker
        .remove_container(&container_name, Some(remove_options))
        .await?;

    if all_passed {
        println!("All steps passed");
    } else {
        println!("Build failed");
        std::process::exit(1);
    }

    Ok(())
    }