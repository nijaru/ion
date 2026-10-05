use std::{
    fs,
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ion_ai::{
    BoxFuture, Content, GenerationControls, Message, ModelRef, ModelRequest, ModelResponse,
    ModelRoute, ModelRouteReason, ModelStreamEvent, PromptCacheIntent, Reasoning,
    ResponseTermination, Role, Script, ScriptedModelService, ToolCall, ToolChoice, ToolSpec, Usage,
};
use ion_core::{
    CodingAgent, CodingSession, CodingToolHost, CodingToolOutput, ToolDefinition, ToolSet,
    TranscriptProjection,
};
use ion_host::SessionCatalog;
use ion_terminal::{Frame, Screen};
use ratatui::text::Line;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const SHORT_TURNS: usize = 4;
const LONG_PREFILL_TURNS: usize = 300;
const MEASURED_TURNS: usize = 20;
const DISCOVERY_SESSIONS: usize = 64;

#[derive(Debug)]
struct Stats {
    min_us: u128,
    p50_us: u128,
    p95_us: u128,
    max_us: u128,
}

fn stats(mut samples: Vec<Duration>) -> Stats {
    samples.sort_unstable();
    let value = |index: usize| samples[index.min(samples.len() - 1)].as_micros();
    Stats {
        min_us: value(0),
        p50_us: value(samples.len() / 2),
        p95_us: value(samples.len() * 95 / 100),
        max_us: value(samples.len() - 1),
    }
}

fn measure_sync<T>(iterations: usize, mut operation: impl FnMut() -> Result<T>) -> Result<Stats> {
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        black_box(operation()?);
        samples.push(started.elapsed());
    }
    Ok(stats(samples))
}

fn print_stats(name: &str, samples: &Stats) {
    println!(
        "{name}: min={}us p50={}us p95={}us max={}us",
        samples.min_us, samples.p50_us, samples.p95_us, samples.max_us
    );
}

fn model() -> ModelRef {
    ModelRef {
        provider: "measure".into(),
        model: "measure-model".into(),
    }
}

fn completion(text: String) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::Text(text)],
            provider_replay: None,
        },
        usage: Usage::known(2_000, 32),
        termination: ResponseTermination::Completed,
        returned_model: Some("measure-model-physical".into()),
    })])
}

struct NoTools;

impl CodingToolHost for NoTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _stop: CancellationToken,
    ) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            CodingToolOutput {
                value: json!({"error": format!("unexpected tool call: {}", call.name)}),
                images: Vec::new(),
                is_error: true,
            }
        })
    }
}

struct CatalogTools {
    count: usize,
    deferred: bool,
}

impl CodingToolHost for CatalogTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        (0..self.count)
            .map(|index| {
                let definition = ToolDefinition::external(ToolSpec {
                    name: format!("mcp__research__search_{index:03}"),
                    description: format!(
                        "Search indexed project and documentation sources for topic {index}; returns bounded structured matches with paths, snippets, ranking metadata and continuation hints."
                    ),
                    input_schema: json!({
                        "type":"object",
                        "additionalProperties":false,
                        "required":["query"],
                        "properties":{
                            "query":{"type":"string","description":"Search query"},
                            "path":{"type":"string","description":"Optional project path scope"},
                            "limit":{"type":"integer","minimum":1,"maximum":100},
                            "include_metadata":{"type":"boolean"}
                        }
                    }),
                });
                if self.deferred {
                    definition.deferred()
                } else {
                    definition
                }
            })
            .collect()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _stop: CancellationToken,
    ) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            CodingToolOutput {
                value: json!({"tool": call.name, "matches":[]}),
                images: Vec::new(),
                is_error: false,
            }
        })
    }
}

async fn fill(
    agent: &CodingAgent,
    session: &CodingSession,
    turns: usize,
    label: &str,
) -> Result<()> {
    for index in 0..turns {
        agent
            .submit(
                session,
                model(),
                format!(
                    "{label} request {index}: inspect the implementation state and preserve the exact constraints needed for continued coding work."
                ),
                "Measure the runtime without changing the workspace.".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await?;
    }
    Ok(())
}

fn request_from(session: &CodingSession) -> Result<ModelRequest> {
    Ok(ModelRequest {
        route: ModelRoute::direct(model(), ModelRouteReason::UserRequest),
        provider_session_id: Some(session.provider_session_id().to_string()),
        instructions: Some("Measure request construction.".into()),
        messages: session.context_messages()?,
        tools: Vec::new(),
        context_timeline: None,
        prompt_cache: PromptCacheIntent::Reusable,
        controls: GenerationControls {
            max_output_tokens: 8_192,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::None,
            parallel_tool_calls: false,
        },
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = std::env::temp_dir().join(format!("ion-runtime-measure-{}", Uuid::now_v7()));
    let workspace = root.join("workspace");
    let state = root.join("state");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&state)?;
    fs::create_dir(workspace.join(".git"))?;

    let result = run(&workspace, &state).await;
    let _ = fs::remove_dir_all(&root);
    result
}

async fn run(workspace: &std::path::Path, state: &std::path::Path) -> Result<()> {
    println!("ion_runtime_measure_v1");

    let catalog = SessionCatalog::new(state.join("sessions"), workspace.to_path_buf());

    let short_service = Arc::new(ScriptedModelService::new(
        (0..SHORT_TURNS).map(|index| completion(format!("short response {index}"))),
    ));
    let short_agent = CodingAgent::new(short_service, Arc::new(NoTools));
    let short = CodingSession::create(catalog.new_path()?, workspace)?;
    fill(&short_agent, &short, SHORT_TURNS, "short").await?;

    let discovery_service = Arc::new(ScriptedModelService::new(
        (0..DISCOVERY_SESSIONS).map(|index| completion(format!("discovery {index}"))),
    ));
    let discovery_agent = CodingAgent::new(discovery_service, Arc::new(NoTools));
    for index in 0..DISCOVERY_SESSIONS {
        let session = CodingSession::create(catalog.new_path()?, workspace)?;
        discovery_agent
            .submit(
                &session,
                model(),
                format!("discovery session {index}"),
                "Measure Session discovery.".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await?;
    }

    let long_scripts = (0..(LONG_PREFILL_TURNS + MEASURED_TURNS))
        .map(|index| {
            completion(format!(
                "long response {index}: recorded implementation findings, constraints, test observations, and next-step details for the coding session."
            ))
        })
        .chain((0..16).map(|index| completion(format!("summary chunk {index}"))));
    let long_service = Arc::new(ScriptedModelService::new(long_scripts));
    let long_agent = CodingAgent::new(long_service, Arc::new(NoTools));
    let long = CodingSession::create(catalog.new_path()?, workspace)?;
    fill(&long_agent, &long, LONG_PREFILL_TURNS, "long").await?;

    let mut turn_samples = Vec::with_capacity(MEASURED_TURNS);
    for index in 0..MEASURED_TURNS {
        let started = Instant::now();
        long_agent
            .submit(
                &long,
                model(),
                format!("measured continuation {index} with representative coding-session context"),
                "Measure the runtime without changing the workspace.".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await?;
        turn_samples.push(started.elapsed());
    }

    let short_view = short.view()?;
    let long_view = long.view()?;
    println!(
        "session_shape: short_entries={} long_entries={} long_messages={} discovered_sessions={}",
        short_view.entries.len(),
        long_view.entries.len(),
        long_view.messages.len(),
        catalog.list()?.len()
    );

    print_stats(
        "transcript_projection_short",
        &measure_sync(500, || Ok(TranscriptProjection::from_session(&short_view)))?,
    );
    print_stats(
        "transcript_projection_long",
        &measure_sync(100, || Ok(TranscriptProjection::from_session(&long_view)))?,
    );
    print_stats("session_view_long", &measure_sync(50, || Ok(long.view()?))?);
    print_stats(
        "context_projection_long",
        &measure_sync(100, || Ok(long.context_messages()?))?,
    );
    print_stats(
        "request_build_encode_long",
        &measure_sync(50, || {
            let request = request_from(&long)?;
            serde_json::to_vec(&request).context("encode request")
        })?,
    );
    print_stats("scripted_coding_step_long", &stats(turn_samples));
    print_stats("session_discovery", &measure_sync(20, || catalog.list())?);

    let live = vec![Line::from("› ready"), Line::from("idle")];
    let frame = Frame {
        live: &live,
        cursor: Some((0, 2)),
    };
    let mut inline = Screen::with_live_height(100, 0, 30, 4);
    let mut output = Vec::new();
    inline.draw(&mut output, &frame)?;
    output.clear();
    print_stats(
        "idle_redraw_inline",
        &measure_sync(10_000, || {
            output.clear();
            inline.draw(&mut output, &frame)?;
            Ok(output.len())
        })?,
    );

    let fullscreen_rows = (0..30)
        .map(|row| Line::from(format!("fullscreen row {row:02}")))
        .collect::<Vec<_>>();
    let mut fullscreen = Screen::new(100, 0, 30);
    fullscreen.draw_fullscreen(&mut output, &fullscreen_rows, Some((29, 2)))?;
    output.clear();
    print_stats(
        "idle_redraw_fullscreen",
        &measure_sync(10_000, || {
            output.clear();
            fullscreen.draw_fullscreen(&mut output, &fullscreen_rows, Some((29, 2)))?;
            Ok(output.len())
        })?,
    );

    for count in [50usize, 100, 250] {
        let direct_host: Arc<dyn CodingToolHost> = Arc::new(CatalogTools {
            count,
            deferred: false,
        });
        let deferred_host: Arc<dyn CodingToolHost> = Arc::new(CatalogTools {
            count,
            deferred: true,
        });
        let direct = ToolSet::new([direct_host]).snapshot();
        let deferred = ToolSet::new([deferred_host]).snapshot();
        let direct_bytes = serde_json::to_vec(&direct.declared_specs())?.len();
        let deferred_bytes = serde_json::to_vec(&deferred.declared_specs())?.len();
        let saved = direct_bytes.saturating_sub(deferred_bytes);
        let percent = if direct_bytes == 0 {
            0.0
        } else {
            saved as f64 * 100.0 / direct_bytes as f64
        };
        println!(
            "tool_loadout_{count}: direct_tools={} deferred_tools={} direct_json_bytes={} deferred_json_bytes={} saved_json_bytes={} saved_percent={percent:.2}",
            direct.declared_specs().len(),
            deferred.declared_specs().len(),
            direct_bytes,
            deferred_bytes,
            saved
        );
    }

    let compaction_started = Instant::now();
    let compacted = long_agent
        .compact(&long, model(), CancellationToken::new(), |_| {})
        .await?;
    println!(
        "compaction_long: changed={} elapsed_us={} entries_after={}",
        compacted,
        compaction_started.elapsed().as_micros(),
        long.view()?.entries.len()
    );

    Ok(())
}
