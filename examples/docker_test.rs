use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder};
use bollard::Docker;
use futures_util::stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let docker = Docker::connect_with_local_defaults()?;

    let config = ContainerCreateBody {
        image: Some("ubuntu:22.04".to_string()),
        cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
        ..Default::default()
    };

    let options = CreateContainerOptionsBuilder::new()
        .name("preflight-ci-test")
        .build();

    let container = docker.create_container(Some(options), config).await?;
    println!("Created container: {}", container.id);

    docker.start_container("preflight-ci-test", None).await?;
    println!("container started");


    let exec = docker
        .create_exec(
            "preflight-ci-test",
            CreateExecOptions {
                cmd: Some(vec!["sh", "-c", "echo hello && sleep 1 && echo space && sleep 3 && ls"]),
                attach_stdout: Some(true),
                attach_stderr: Some(true),  
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

    let remove_options = RemoveContainerOptionsBuilder::new().force(true).build();
    docker
        .remove_container("preflight-ci-test", Some(remove_options))
        .await?;
    println!("Container removed");

    Ok(())
}