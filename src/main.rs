// SPDX-License-Identifier: MIT
use agent_runtime::{
    app::{Runtime, RuntimeOptions, WorkDir, read_bounded, required_str, save_private_new},
    tools,
};
use runtime_core::{Result, resources::private_dir};
use runtime_engine::{CodexClient, Input, TurnStatus};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
};

const MAX_LINE: usize = 1024 * 1024;

fn emit(value: &Value) -> Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value).map_err(|e| e.to_string())?;
    out.write_all(b"\n").map_err(|e| e.to_string())
}

struct Options {
    state: PathBuf,
    account: String,
    cookie: Option<PathBuf>,
    tieba_bduss: Option<PathBuf>,
    context: runtime_jobs::AgentContext,
    mind_state: Option<PathBuf>,
    args: Vec<String>,
}
impl Options {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1).peekable();
        let mut state = std::env::var_os("AGENT_RUNTIME_STATE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".runtime/state"));
        let mut account = "public".to_string();
        let mut cookie = None;
        let mut tieba_bduss = None;
        let mut context = runtime_jobs::AgentContext::default();
        let mut mind_state = None;
        while args.peek().is_some_and(|v| v.starts_with("--")) {
            let flag = args.next().unwrap();
            match flag.as_str() {
                "--state" => state = PathBuf::from(args.next().ok_or("missing state path")?),
                "--account" => account = args.next().ok_or("missing account alias")?,
                "--persona" => context.persona = args.next().ok_or("missing persona ID")?,
                "--mind-state" => {
                    mind_state = Some(PathBuf::from(args.next().ok_or("missing mind state path")?))
                }
                "--agent" => context.agent = args.next().ok_or("missing agent ID")?,
                "--conversation" => {
                    context.conversation = Some(args.next().ok_or("missing conversation ID")?)
                }
                "--bili-cookie-file" => {
                    cookie = Some(PathBuf::from(args.next().ok_or("missing Cookie file")?))
                }
                "--tieba-bduss-file" => {
                    tieba_bduss = Some(PathBuf::from(args.next().ok_or("missing BDUSS file")?))
                }
                "--help" => {
                    return Ok(Self {
                        state,
                        account,
                        cookie,
                        tieba_bduss,
                        context,
                        mind_state,
                        args: vec!["help".into()],
                    });
                }
                "--version" => {
                    return Ok(Self {
                        state,
                        account,
                        cookie,
                        tieba_bduss,
                        context,
                        mind_state,
                        args: vec!["version".into()],
                    });
                }
                _ => return Err("unknown global option".into()),
            }
        }
        context.validate()?;
        if mind_state.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err("mind state must be an absolute path".into());
        }
        Ok(Self {
            state,
            account,
            cookie,
            tieba_bduss,
            context,
            mind_state,
            args: args.collect(),
        })
    }
    fn runtime(&self) -> Result<Runtime> {
        let mut runtime = Runtime::open_with_options(
            &self.state,
            &self.account,
            RuntimeOptions {
                bili_cookie_file: self.cookie.as_deref(),
                tieba_bduss_file: self.tieba_bduss.as_deref(),
            },
        )?;
        runtime.set_agent_context(self.context.clone())?;
        if let Some(state) = &self.mind_state {
            runtime.set_mind_state(state)?;
        }
        Ok(runtime)
    }
}

fn parse_params(args: &[String], mut value: Value) -> Result<Value> {
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let key = match flag.as_str() {
            "--page" => "page",
            "--start-ms" => "start_ms",
            "--duration-ms" => "duration_ms",
            "--interval-ms" => "interval_ms",
            "--max-edge" => "max_edge",
            "--offset" => "offset",
            "--limit" => "limit",
            "--audio" => "audio_resource",
            "--pointer" => "pointer",
            "--encoding" => "encoding",
            "--post" => "post",
            "--sequence" => "sequence",
            "--kind" => "kind",
            "--cursor-id" => "cursor_id",
            "--cursor-time" => "cursor_time",
            _ => return Err("unknown command option".into()),
        };
        let arg = args.next().ok_or("missing command option value")?;
        value[key] = if matches!(key, "audio_resource" | "pointer" | "encoding" | "kind") {
            json!(arg)
        } else {
            json!(arg.parse::<u64>().map_err(|_| "invalid integer option")?)
        };
    }
    Ok(value)
}

fn serve(runtime: &mut Runtime) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    loop {
        let mut line = Vec::new();
        let n = Read::by_ref(&mut reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        if line.len() > MAX_LINE {
            return Err("JSONL request exceeds 1 MiB".into());
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let request: Value = match serde_json::from_slice(&line) {
            Ok(v) => v,
            Err(_) => {
                emit(&json!({"id":null,"error":"invalid JSON request"}))?;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if !(id.is_null() || id.is_string() || id.is_i64() || id.is_u64()) {
            emit(&json!({"id":null,"error":"id must be a string or integer"}))?;
            continue;
        }
        let result = required_str(&request, "method")
            .and_then(|method| runtime.call(method, request.get("params").unwrap_or(&json!({}))));
        let response = match result {
            Ok(v) => json!({"id":id,"result":v}),
            Err(e) => json!({"id":id,"error":e}),
        };
        let encoded = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
        if encoded.len() > MAX_LINE {
            emit(&json!({"id":id,"error":"result exceeds 1 MiB; request a smaller range"}))?;
        } else {
            emit(&response)?;
        }
    }
}

fn engine(options: &Options, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("capabilities");
    if action == "mcp-bridge" && args.len() == 2 {
        return agent_runtime::engine_bridge::run_stdio_proxy(Path::new(&args[1]));
    }
    if action == "capabilities" {
        return emit(
            &json!({"engines":[runtime_engine::codex_capabilities(),runtime_engine::Antigravity::capabilities()]}),
        );
    }
    if !matches!(action, "probe" | "run") {
        return Err("engine action must be capabilities, probe or run".into());
    }
    private_dir(&options.state)?;
    let state = options.state.canonicalize().map_err(|e| e.to_string())?;
    let mut binary =
        agent_runtime::deployment::native_binary(agent_runtime::agent_host::EngineKind::Codex);
    let mut engine_kind = agent_runtime::agent_host::EngineKind::Codex;
    let mut binary_selected = false;
    let mut home_selected = false;
    let mut home = state.join("codex");
    let mut workspace = state.join("agent-workspace");
    let mut prompt = None;
    let mut images = Vec::new();
    let mut thread = None;
    let mut model = None;
    let mut model_catalog = None;
    let mut allowed_tools = None;
    let mut result_file = None;
    let mut args = args[1..].iter();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("missing engine option value")?;
        match flag.as_str() {
            "--engine" => {
                engine_kind = match value.as_str() {
                    "codex" => agent_runtime::agent_host::EngineKind::Codex,
                    "antigravity" => agent_runtime::agent_host::EngineKind::Antigravity,
                    _ => return Err("engine must be codex or antigravity".into()),
                }
            }
            "--binary" => {
                binary = PathBuf::from(value);
                binary_selected = true;
            }
            "--home" => {
                home = PathBuf::from(value);
                home_selected = true;
            }
            "--workspace" => workspace = PathBuf::from(value),
            "--prompt" => prompt = Some(value.clone()),
            "--image" => images.push(value.clone()),
            "--thread" => thread = Some(value.clone()),
            "--model" => model = Some(value.clone()),
            "--model-catalog" => model_catalog = Some(PathBuf::from(value)),
            "--tools" => {
                allowed_tools = Some(
                    value
                        .split(',')
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                )
            }
            "--output" => result_file = Some(PathBuf::from(value)),
            _ => return Err("unknown engine option".into()),
        }
    }
    private_dir(&workspace)?;
    if engine_kind == agent_runtime::agent_host::EngineKind::Antigravity {
        if !binary_selected {
            binary = agent_runtime::deployment::native_binary(
                agent_runtime::agent_host::EngineKind::Antigravity,
            );
        }
        if !home_selected {
            home = state.join("antigravity");
        }
    }
    let settings = agent_runtime::agent_host::Settings {
        engine: engine_kind,
        binary,
        model: model.clone(),
        allowed_tools,
        model_catalog,
        ..Default::default()
    };
    if engine_kind == agent_runtime::agent_host::EngineKind::Antigravity {
        if thread.is_some() || !images.is_empty() {
            return Err("Antigravity currently accepts text and portable --conversation context; native thread/media input is not verified".into());
        }
        let mut config = settings.antigravity_config(&state)?;
        config.home = home;
        config.workspace = workspace;
        if action == "probe" {
            return emit(&agent_runtime::agent_antigravity::probe(config)?);
        }
        let prompt = prompt.ok_or("engine run requires --prompt")?;
        let mut runtime = options.runtime()?;
        let queue = runtime_jobs::Queue::open(&state)?;
        queue.recover_interrupted(agent_runtime::agent_host::now()?)?;
        let id = format!(
            "engine-agy-{}-{}",
            agent_runtime::agent_host::now()?,
            std::process::id()
        );
        queue.enqueue_with_context(
            &id,
            &prompt,
            None,
            options.context.clone(),
            agent_runtime::agent_host::now()?,
        )?;
        let report = agent_runtime::agent_antigravity::run_job_controlled(
            &mut runtime,
            &queue,
            config,
            &std::sync::atomic::AtomicBool::new(false),
            settings.allowed_tools.as_deref(),
            &id,
        )?;
        let job = queue.get(&id)?;
        let value = json!({"engine":"antigravity","report":report,"job":job,"model_session_created":report.thread_id.is_some()});
        if let Some(path) = result_file {
            save_private_new(&path, &value)?;
        }
        emit(&value)?;
        if report.status != "completed" {
            return Err("Antigravity request did not complete; inspect the recorded job. No automatic replay.".into());
        }
        return Ok(());
    }
    let specs = agent_runtime::agent_host::selected_tools(settings.allowed_tools.as_deref())?;
    let mut config = settings.engine_config(&state)?;
    config.codex_home = home;
    config.workspace = workspace.clone();
    let prepared = if action == "run" {
        Some(agent_runtime::agent_mind::assemble_with_store(
            &state,
            options.mind_state.as_deref().unwrap_or(&state),
            &options.context,
            prompt.as_deref().ok_or("engine run requires --prompt")?,
            None,
            None,
        )?)
    } else {
        None
    };
    if let Some(prepared) = &prepared {
        if thread.is_some()
            && (options.context != runtime_jobs::AgentContext::default()
                || prepared.version.persona_revision > 0
                || prepared.version.role_revision > 0
                || prepared.version.memory_revision > 0)
        {
            return Err("Raw --thread reuse cannot validate portable persona/memory context; use --conversation ID for Runtime-managed recent dialogue".into());
        }
        config.developer_instructions = Some(prepared.developer_instructions.clone());
    }
    let mut client = CodexClient::spawn(config).map_err(|e| e.to_string())?;
    let server = client.initialize().map_err(|e| e.to_string())?;
    let account = client.account_read().map_err(|e| e.to_string())?;
    if action == "probe" {
        return emit(&json!({"server":server,"account":account,"model_turn_started":false}));
    }
    let prompt = prompt.ok_or("engine run requires --prompt")?;
    if !account.authenticated {
        return Err(
            "Sign in to Codex in the dedicated CODEX_HOME first; no model turn was started".into(),
        );
    }
    let mut runtime = options.runtime()?;
    let prepared = prepared.ok_or("engine context missing")?;
    let mut inputs = prepared.inputs;
    if images.len() > 8 {
        return Err("at most eight explicit image inputs per turn".into());
    }
    let image_work = WorkDir::new(&workspace)?;
    for (index, id) in images.iter().enumerate() {
        let source = runtime.image_path(id)?;
        let output = image_work.path.join(format!("input-{index}.jpg"));
        let mut limits = runtime_media::Limits::default();
        limits.max_output_bytes = 512 * 1024;
        runtime_media::pictures::prepare_image(
            &source,
            &output,
            1280,
            runtime_media::pictures::OutputFormat::Jpeg,
            &limits,
        )
        .map_err(|e| e.to_string())?;
        inputs.push(Input::LocalImage { path: output });
    }
    let handle = match thread {
        Some(id) => client.thread_resume_with_tools(&id, &specs),
        None => client.thread_start_with_tools(model.as_deref(), &specs),
    }
    .map_err(|e| e.to_string())?;
    let mut tool_calls = 0_u32;
    let mut tool_errors = 0_u32;
    let mut image_items = 0_usize;
    let output = client
        .turn_start_with_tools(
            &handle.id,
            &inputs,
            |event| {
                if let Err(error) = emit(&json!({"engine_event":event})) {
                    eprintln!("{error}");
                }
            },
            |call| {
                tool_calls += 1;
                let result = runtime.dispatch_engine_tool(&call);
                tool_errors += u32::from(!result.success);
                image_items += result
                    .content_items
                    .iter()
                    .filter(|item| matches!(item, runtime_engine::ToolContent::InputImage { .. }))
                    .count();
                Ok(result)
            },
        )
        .map_err(|e| e.to_string())?;
    let history_saved = if options.context.conversation.is_some() {
        Some(
            agent_runtime::agent_dialogue::Store::open(&state)?
                .append_at_epoch(
                    &options.context,
                    agent_runtime::agent_dialogue::Turn {
                        execution_id: Some(format!(
                            "run-{}",
                            agent_runtime::agent_mind::stable_key(&[
                                output.thread_id.as_bytes(),
                                output.turn_id.as_bytes()
                            ])
                        )),
                        job_id: format!(
                            "engine-{}",
                            agent_runtime::agent_mind::stable_key(&[
                                output.thread_id.as_bytes(),
                                output.turn_id.as_bytes()
                            ])
                        ),
                        at: agent_runtime::agent_host::now()?,
                        status: serde_json::to_value(output.status)
                            .map_err(|e| e.to_string())?
                            .as_str()
                            .ok_or("invalid turn status")?
                            .into(),
                        user: agent_runtime::agent_mind::clip(&prompt, 2048).into(),
                        assistant: Some(agent_runtime::agent_mind::clip(&output.text, 2048).into()),
                        operation_ids: Vec::new(),
                        operation_count: 0,
                    },
                    prepared.version.history_epoch,
                )
                .is_ok(),
        )
    } else {
        None
    };
    let value = json!({"turn":output,"context":options.context,"context_version":prepared.version,"history_persisted":history_saved,"tool_calls":tool_calls,"tool_errors":tool_errors,"real_image_items":image_items,"max_rss_kib":runtime_core::peak_rss_kib(),"audio_understanding":false});
    if let Some(path) = result_file {
        save_private_new(&path, &value)?;
    }
    emit(&value)?;
    if output.status != TurnStatus::Completed {
        return Err("model turn did not complete successfully".into());
    }
    Ok(())
}

fn run() -> Result<()> {
    let options = Options::parse()?;
    let args = &options.args;
    let command = args.first().map(String::as_str).unwrap_or("help");
    match command {
        "version" => emit(&json!({"name":"Glimlink","version":env!("CARGO_PKG_VERSION")})),
        "doctor" => emit(&agent_runtime::deployment::doctor(&options.state)),
        "help" => {
            println!(
                "Agent Runtime\n  operator serve                 Private local/SSH control stdio\n  speech status|install           Optional pinned Rust Whisper model\n  modules\n  tools\n  serve                          Compatibility JSONL Runtime routes\n  mcp                            Standard MCP stdio task tools\n  call METHOD JSON\n  import FILE MIME\n  content html|json FILE [--pointer /path]\n  bili videos [--page N --limit N]\n  bili media ASSET\n  bili unread\n  bili notifications reply|at|like [--limit N --cursor-id N --cursor-time N]\n  bili import-login PRIVATE_FILE | login|login-poll|status|me|favorites\n  bili inspect|fetch BV_OR_AV [--page N]\n  tieba posts [--kind thread|reply --page N --limit N]\n  tieba notifications reply|at|like [--page N --limit N]\n  tieba import-login PRIVATE_FILE | login|login-poll|status|me\n  tieba thread THREAD [--page N]\n  tieba comments THREAD --post POST [--page N]\n  tieba media ASSET\n  qq password-login PRIVATE_JSON [--resume-from-disk] | import-login PRIVATE_JSON\n  qq import DIRECTORY|login|qr [PROFILE]|status|signer-check\n  github login TOKEN_FILE|status\n  x import-login PRIVATE_JSON|status|me|open URL_OR_HANDLE|posts [--limit N]\n  weixin login|login-poll|status|resume|contacts|receive|inbox\n  weixin media ASSET\n  media image|audio|video RESOURCE [--start-ms N --duration-ms N --interval-ms N --audio RESOURCE]\n  resource read|list|drop [RESOURCE] [--offset N --limit N --encoding utf8|base64]\n  asset list [--offset N --limit N]\n  asset drop ASSET --kind bilibili-media|tieba-media|weixin-media|weixin-payload\n  agent status|jobs|job|enqueue|watch|watches|resume|cancel|gc|run\n  mind status|context|persona|role|remember|forget|search|conversation-reset\n  engine capabilities|probe|run [--engine codex|antigravity --binary PATH --home PATH --workspace PATH --prompt TEXT --image RESOURCE --tools NAME,...]\nGlobal options precede the command: --state PATH --account ALIAS --persona ID --agent ID --conversation ID --mind-state ABS_PATH --bili-cookie-file PATH --tieba-bduss-file PATH"
            );
            Ok(())
        }
        "modules" => emit(&json!({"modules":agent_runtime::registry::modules()})),
        "tools" => emit(&json!({"tools":tools::specs()})),
        "operator" if args.len() == 2 && args[1] == "serve" => {
            agent_runtime::operator_control::serve(
                &options.state,
                io::stdin().lock(),
                io::stdout().lock(),
            )
        }
        "operator" if args.len() == 2 && args[1] == "manage" => {
            agent_runtime::control_console::serve(&options.state, io::stdin().lock(), io::stdout().lock())
        }
        "operator" if args.len()==2 && matches!(args[1].as_str(),"worker-start"|"worker-stop"|"worker-status") => emit(&json!({"ok":false,"code":"worker_control_unavailable"})),
        "engine" => engine(&options, &args[1..]),
        "speech" => {
            let directory = agent_runtime::app::speech_model_directory(&options.state)?;
            let result = match args.get(1).map(String::as_str).unwrap_or("status") {
                "status" if args.len() <= 2 => runtime_speech::status(&directory)?,
                "install" if args.len() == 2 => runtime_speech::install(&directory)?,
                _ => return Err("speech status | install".into()),
            };
            emit(&serde_json::to_value(result).map_err(|e| e.to_string())?)
        }
        "agent" => {
            if options.cookie.is_some() || options.tieba_bduss.is_some() {
                return Err("Agent mode uses managed saved platform accounts; temporary credential-file overrides are unsupported".into());
            }
            emit(&agent_runtime::agent_cli::command_with_environment(
                &options.state,
                &options.account,
                &args[1..],
                &options.context,
                options.mind_state.as_deref(),
            )?)
        }
        "mind" => emit(&agent_runtime::agent_mind::command_with_store(
            &options.state,
            options.mind_state.as_deref().unwrap_or(&options.state),
            &options.context,
            &args[1..],
        )?),
        _ => {
            let mut runtime = options.runtime()?;
            let result = match command {
                "serve" => return serve(&mut runtime),
                "mcp" => {
                    return agent_runtime::mcp::serve(
                        &mut runtime,
                        io::stdin().lock(),
                        io::stdout().lock(),
                    );
                }
                "call" => {
                    if args.len() != 3 {
                        return Err("call needs METHOD and one JSON object".into());
                    }
                    if args[2].len() > MAX_LINE {
                        return Err("tool parameters exceed limit".into());
                    }
                    let params = serde_json::from_str(&args[2]).map_err(|_| "invalid tool JSON")?;
                    runtime.call(&args[1], &params)?
                }
                "import" => {
                    if args.len() != 3 {
                        return Err("import needs FILE MIME".into());
                    }
                    json!({"resource":runtime.import_file(Path::new(&args[1]),&args[2])?})
                }
                "qq" => match args.get(1).map(String::as_str) {
                    Some("password-login") if args.len() == 4 && args[3] == "--resume" => {
                        return Err("Use agentctl qq login --method password to complete verification on the original connection. Standalone reconnect requires explicit --resume-from-disk.".into());
                    }
                    Some("password-login")
                        if args.len() == 4 && args[3] == "--resume-from-disk" =>
                    {
                        runtime.qq_password_resume_from_disk(Path::new(&args[2]))?
                    }
                    Some("qr") if args.len() == 2 => runtime.qq_qr()?,
                    Some("qr") if args.len() == 3 => runtime.qq_qr_with_profile(Some(&args[2]))?,
                    Some("signer-check") if args.len() == 2 => runtime.qq_signer_check()?,
                    Some("import") if args.len() == 3 => runtime.qq_import(Path::new(&args[2]))?,
                    Some("password-login") if args.len() == 3 => {
                        runtime.qq_password_login(Path::new(&args[2]))?
                    }
                    Some("import-login") if args.len() == 3 => {
                        runtime.qq_import_login(Path::new(&args[2]))?
                    }
                    Some("login") if args.len() == 2 => runtime.qq_login()?,
                    Some("status") if args.len() == 2 => runtime.call("qq.status", &json!({}))?,
                    _ => {
                        return Err(
                            "qq needs password-login PRIVATE_JSON, import-login PRIVATE_JSON, import DIRECTORY, login, qr, signer-check or status".into(),
                        );
                    }
                },
                "github" => match args.get(1).map(String::as_str) {
                    Some("login") if args.len() == 3 => {
                        runtime.github_login(Path::new(&args[2]))?
                    }
                    Some("status") if args.len() == 2 => {
                        runtime.call("github.status", &json!({}))?
                    }
                    _ => return Err("github needs login TOKEN_FILE or status".into()),
                },
                "x" | "twitter" => match args.get(1).map(String::as_str) {
                    Some("resume-import") if args.len()==2 => runtime.x_resume_import()?,
                    Some("import-login") if args.len()==3 => runtime.x_import_login(Path::new(&args[2]))?,
                    Some("status"|"me") if args.len()==2 => runtime.call(&format!("x.{}",args[1]),&json!({}))?,
                    Some("open") if args.len()==3 => runtime.call("social.open",&json!({"platform":"x","reference":args[2]}))?,
                    Some("posts") => runtime.call("social.publications",&parse_params(&args[2..],json!({"platform":"x"}))?)?,
                    _ => return Err("x needs import-login PRIVATE_JSON, status, me, open URL_OR_HANDLE, or posts [--limit N]".into()),
                },
                "content" => {
                    let format = args.get(1).ok_or("content needs html|json and FILE")?;
                    if !matches!(format.as_str(), "html" | "json") {
                        return Err("content format must be html or json".into());
                    }
                    let path = Path::new(args.get(2).ok_or("missing content file")?);
                    read_bounded(path, runtime_content::MAX_INPUT as u64)?;
                    let resource = runtime.import_file(
                        path,
                        if format == "html" {
                            "text/html"
                        } else {
                            "application/json"
                        },
                    )?;
                    let params = parse_params(&args[3..], json!({"resource":resource.id}))?;
                    runtime.call(&format!("content.{format}"), &params)?
                }
                "bili" => {
                    let action = args
                        .get(1)
                        .ok_or("bili needs login|login-poll|status|me|favorites|inspect|fetch")?;
                    if action == "import-login" {
                        if args.len() != 3 {
                            return Err("bili import-login needs PRIVATE_FILE".into());
                        }
                        runtime.bilibili_import_login(Path::new(&args[2]))?
                    } else if action == "videos" {
                        runtime.call(
                            "bilibili.publications",
                            &parse_params(&args[2..], json!({}))?,
                        )?
                    } else if action == "media" {
                        if args.len() != 3 {
                            return Err("bili media needs one asset ID".into());
                        }
                        runtime.call("bilibili.media", &json!({"asset":args[2]}))?
                    } else if action == "notifications" {
                        let kind = args.get(2).ok_or("notifications need reply|at|like")?;
                        runtime.call(
                            "bilibili.notifications",
                            &parse_params(&args[3..], json!({"kind":kind}))?,
                        )?
                    } else if action == "unread" {
                        if args.len() != 2 {
                            return Err("unread accepts no arguments".into());
                        }
                        runtime.call("bilibili.unread", &json!({}))?
                    } else {
                        if matches!(
                            action.as_str(),
                            "login" | "login-poll" | "status" | "me" | "favorites"
                        ) {
                            if args.len() != 2 {
                                return Err("account actions accept no extra arguments".into());
                            }
                            let result = match action.as_str() {
                                "login" => runtime.bilibili_login()?,
                                "login-poll" => runtime.bilibili_poll_login()?,
                                _ => runtime.call(&format!("bilibili.{action}"), &json!({}))?,
                            };
                            return emit(
                                &json!({"result":result,"max_rss_kib":runtime_core::peak_rss_kib()}),
                            );
                        }
                        if !matches!(action.as_str(), "inspect" | "fetch") {
                            return Err("unknown Bilibili action".into());
                        }
                        let id = args.get(2).ok_or("missing video ID")?;
                        runtime.call(
                            &format!("bilibili.{action}"),
                            &parse_params(&args[3..], json!({"video":id}))?,
                        )?
                    }
                }
                "tieba" => {
                    let action = args
                        .get(1)
                        .ok_or("tieba needs login|login-poll|status|me|thread|comments|media")?;
                    if action == "import-login" {
                        if args.len() != 3 {
                            return Err("tieba import-login needs PRIVATE_FILE".into());
                        }
                        runtime.tieba_import_login(Path::new(&args[2]))?
                    } else if action == "posts" {
                        runtime.call("tieba.publications", &parse_params(&args[2..], json!({}))?)?
                    } else if action == "notifications" {
                        let kind = args.get(2).ok_or("notifications need reply|at|like")?;
                        runtime.call(
                            "tieba.notifications",
                            &parse_params(&args[3..], json!({"kind":kind}))?,
                        )?
                    } else {
                        if matches!(action.as_str(), "login" | "login-poll" | "status" | "me") {
                            if args.len() != 2 {
                                return Err("account actions accept no extra arguments".into());
                            }
                            let result = match action.as_str() {
                                "login" => runtime.tieba_login()?,
                                "login-poll" => runtime.tieba_poll_login()?,
                                _ => runtime.call(&format!("tieba.{action}"), &json!({}))?,
                            };
                            return emit(
                                &json!({"result":result,"max_rss_kib":runtime_core::peak_rss_kib()}),
                            );
                        }
                        let target = args.get(2).ok_or("missing thread/asset ID")?;
                        if !matches!(action.as_str(), "thread" | "comments" | "media") {
                            return Err("unknown Tieba action".into());
                        }
                        let params = if action == "media" {
                            json!({"asset":target})
                        } else {
                            json!({"thread":target})
                        };
                        runtime.call(
                            &format!("tieba.{action}"),
                            &parse_params(&args[3..], params)?,
                        )?
                    }
                }
                "weixin" => {
                    let action = args.get(1).ok_or(
                        "weixin needs login|login-poll|status|resume|contacts|receive|inbox|media",
                    )?;
                    match action.as_str() {
                        "login" => {
                            if args.len() == 2 {
                                runtime.weixin_login()?
                            } else if args.len() == 4 && args[2] == "--profile" {
                                runtime.weixin_login_profile(&args[3])?
                            } else {
                                return Err(
                                    "weixin login accepts only --profile web|desktop".into()
                                );
                            }
                        }
                        "login-poll" => runtime.weixin_poll_login()?,
                        "status" | "resume" | "contacts" | "receive" | "inbox" => runtime.call(
                            &format!("weixin.{action}"),
                            &parse_params(&args[2..], json!({}))?,
                        )?,
                        "media" => runtime.call(
                            "weixin.media",
                            &json!({"asset":args.get(2).ok_or("missing media asset ID")?}),
                        )?,
                        _ => return Err("unknown Weixin action".into()),
                    }
                }
                "asset" => {
                    if args.get(1).map(String::as_str) == Some("list") {
                        let result =
                            runtime.call("asset.list", &parse_params(&args[2..], json!({}))?)?;
                        return emit(&json!({"result":result}));
                    }
                    if args.get(1).map(String::as_str) != Some("drop") {
                        return Err("asset supports drop ASSET --kind KIND".into());
                    }
                    runtime.call(
                        "asset.drop",
                        &parse_params(
                            &args[3..],
                            json!({"asset":args.get(2).ok_or("missing asset ID")?}),
                        )?,
                    )?
                }
                "media" => {
                    let kind = args.get(1).ok_or("media needs image|gif|audio|video")?;
                    if !matches!(kind.as_str(), "image" | "audio" | "video" | "gif") {
                        return Err("unknown media type".into());
                    }
                    let resource = args.get(2).ok_or("missing resource ID")?;
                    runtime.call(
                        &format!("media.{kind}"),
                        &parse_params(&args[3..], json!({"resource":resource}))?,
                    )?
                }
                "resource" => {
                    let action = args.get(1).ok_or("resource needs read|list|drop")?;
                    if action == "list" {
                        runtime.call("resource.list", &parse_params(&args[2..], json!({}))?)?
                    } else if matches!(action.as_str(), "read" | "drop") {
                        runtime.call(
                            &format!("resource.{action}"),
                            &parse_params(
                                &args[3..],
                                json!({"resource":args.get(2).ok_or("missing resource ID")?}),
                            )?,
                        )?
                    } else {
                        return Err("unknown resource action".into());
                    }
                }
                _ => return Err("unknown command; use --help".into()),
            };
            emit(&json!({"result":result,"max_rss_kib":runtime_core::peak_rss_kib()}))
        }
    }
}

fn main() {
    if let Err(error) = run() {
        let _ = emit(&json!({"error":error}));
        std::process::exit(1);
    }
}
