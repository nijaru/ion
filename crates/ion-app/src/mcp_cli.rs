//! Offline MCP configuration commands; active clients retain captured connections.
use anyhow::Result;
use ion_host::{McpConfig, McpHttpServer, McpServer, McpStdioServer};

use crate::{cli::McpAction, metadata_field, print_stderr, print_stdout};

pub(super) fn run(action: McpAction, config: &McpConfig) -> Result<()> {
    match action {
        McpAction::Enable { name, project } => set_enabled(config, &name, true, project)?,
        McpAction::Disable { name, project } => set_enabled(config, &name, false, project)?,
        McpAction::List => {
            if let Some(path) = config.project_path().filter(|path| path.is_file()) {
                print_stderr(format_args!(
                    "[mcp project: {}]",
                    metadata_field(&path.to_string_lossy())
                ))?;
            }
            let listing = config.list()?;
            for diagnostic in listing.diagnostics {
                print_stderr(format_args!(
                    "[mcp config: {}]",
                    metadata_field(&diagnostic)
                ))?;
            }
            for (name, server) in listing.servers {
                let exposure = if server.enabled() {
                    server.exposure().as_str()
                } else {
                    "disabled"
                };
                match &server {
                    McpServer::Stdio(server) => {
                        print_stdout(format_args!(
                            "{}\t{}\t{} {}",
                            metadata_field(&name),
                            exposure,
                            metadata_field(&server.command),
                            metadata_field(&server.args.join(" "))
                        ))?;
                        if let Some(cwd) = &server.cwd {
                            print_stdout(format_args!(
                                "  cwd\t{}",
                                metadata_field(&cwd.to_string_lossy())
                            ))?;
                        }
                        for name in server.env.keys() {
                            print_stdout(format_args!("  env\t{}", metadata_field(name)))?;
                        }
                    }
                    McpServer::Http(server) => {
                        print_stdout(format_args!(
                            "{}\t{}\t{}",
                            metadata_field(&name),
                            exposure,
                            metadata_field(&server.url)
                        ))?;
                        for name in server.headers.keys() {
                            print_stdout(format_args!("  header\t{}", metadata_field(name)))?;
                        }
                    }
                }
                if !server.enabled() {
                    print_stdout(format_args!("  exposure\t{}", server.exposure().as_str()))?;
                }
                for (tool, exposure) in server.tool_exposure() {
                    print_stdout(format_args!(
                        "  tool\t{}\t{}",
                        metadata_field(tool),
                        exposure.as_str()
                    ))?;
                }
            }
        }
        McpAction::Add {
            name,
            command,
            project,
            exposure,
            server_cwd,
            env,
            args,
        } => {
            let server = McpServer::Stdio(McpStdioServer {
                enabled: true,
                cwd: server_cwd,
                env: env.into_iter().collect(),
                command,
                args,
                exposure: exposure.into(),
                tool_exposure: Default::default(),
            });
            if project {
                config.add_project(&name, server)?;
                print_stdout(format_args!(
                    "Added project MCP server {}; run `ion trust` to enable it",
                    metadata_field(&name)
                ))?;
            } else {
                config.add(&name, server)?;
                print_stdout(format_args!("Added MCP server {}", metadata_field(&name)))?;
            }
        }
        McpAction::AddHttp {
            name,
            url,
            bearer_token_env,
            headers,
            project,
            exposure,
        } => {
            let server = McpServer::Http(McpHttpServer {
                enabled: true,
                url,
                bearer_token_env,
                headers: headers.into_iter().collect(),
                exposure: exposure.into(),
                tool_exposure: Default::default(),
            });
            if project {
                config.add_project(&name, server)?;
                print_stdout(format_args!(
                    "Added project MCP server {}; run `ion trust` to enable it",
                    metadata_field(&name)
                ))?;
            } else {
                config.add(&name, server)?;
                print_stdout(format_args!("Added MCP server {}", metadata_field(&name)))?;
            }
        }
        McpAction::Remove { name, project } => {
            if project {
                config.remove_project(&name)?;
                print_stdout(format_args!(
                    "Removed project MCP server {}",
                    metadata_field(&name)
                ))?;
            } else {
                config.remove(&name)?;
                print_stdout(format_args!("Removed MCP server {}", metadata_field(&name)))?;
            }
        }
    }
    Ok(())
}

fn set_enabled(config: &McpConfig, name: &str, enabled: bool, project: bool) -> Result<()> {
    if project {
        config.set_project_enabled(name, enabled)?;
    } else {
        config.set_enabled(name, enabled)?;
    }
    let action = if enabled { "Enabled" } else { "Disabled" };
    let scope = if project { "project" } else { "user" };
    print_stdout(format_args!(
        "{action} {scope} MCP server {} for new clients{}",
        metadata_field(name),
        if project {
            "; run `ion trust` if the project contents changed"
        } else {
            ""
        }
    ))
}
