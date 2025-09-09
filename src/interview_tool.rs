use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use chrono::Local;

use rmcp::{
    RoleServer, ServerHandler,
    handler::server::{router::prompt::PromptRouter, tool::ToolRouter, wrapper::Parameters},
    model::{
        AnnotateAble, CallToolResult, Content, ErrorCode, ErrorData as McpError,
        GetPromptRequestParam, GetPromptResult, ListPromptsResult, ListResourcesResult,
        PaginatedRequestParam, PromptMessage, RawResource, Resource, ServerCapabilities,
        ServerInfo,
    },
    prompt, prompt_handler, prompt_router,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::AsyncWriteExt,
    process::Command,
    time::{self, Instant},
};

#[derive(Clone)]
pub struct InterviewTool {
    instant_map: Arc<RwLock<HashMap<String, InstantInfo>>>,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}
struct InstantInfo {
    start_instance: Instant,
    label: String,
}
impl InstantInfo {
    fn new(now: Instant, label: &str) -> Self {
        Self {
            start_instance: now,
            label: label.to_string(),
        }
    }
}

#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
struct CreateInstantArgs {
    label: String,
}
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
struct QueryInstantArgs {
    instance_id: String,
}
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
struct ReadFileArgs {
    file_path: String,
}
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
struct CreateFileArgs {
    file_path: String,
    context: String,
}
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
struct CmdArgs {
    cmd: String,
}
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
struct DirInfo {
    name: DirName,
}
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
enum DirName {
    Downloads,
    Documents,
}
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
struct GetUrlArgs {
    url: String,
}
#[tool_router]
impl InterviewTool {
    pub fn new() -> Self {
        Self {
            instant_map: Arc::new(RwLock::new(HashMap::new())),
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }
    fn _create_resource_text(&self, uri: &str, name: &str) -> Resource {
        RawResource::new(uri, name.to_string()).no_annotation()
    }
    #[tool(description = "获取当前时间，格式为 'YYYY-MM-DD HH:MM:SS'")]
    async fn current_time(&self) -> Result<CallToolResult, McpError> {
        let now = Local::now();
        let time_formated = now.format("%Y-%m-%d %H:%M:%S").to_string();
        Ok(CallToolResult::success(vec![Content::text(time_formated)]))
    }
    #[tool(description = "记录一个带标签的时间点，返回 instance_id。")]
    async fn create_instant(
        &self,
        Parameters(args): Parameters<CreateInstantArgs>,
    ) -> Result<CallToolResult, McpError> {
        let uuid = uuid::Uuid::new_v4().to_string();
        let instant_map = self.instant_map.clone();
        let mut guard = instant_map.write().map_err(|err| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to acquired write lock :{}", err),
                None,
            )
        })?;
        guard.insert(uuid.clone(), InstantInfo::new(Instant::now(), &args.label));

        Ok(CallToolResult::success(vec![Content::text(format!(
            "instance uuid is {}",
            uuid
        ))]))
    }
    #[tool(
        description = "计算自指定时间点以来经过的时间。需要提供 instance_id，返回 'mm:ss' 格式。可用于检查用户的回答是否超时。"
    )]
    async fn elapsed_since(
        &self,
        Parameters(args): Parameters<QueryInstantArgs>,
    ) -> Result<CallToolResult, McpError> {
        fn format_duration_mm_ss(d: time::Duration) -> String {
            let total_secs = d.as_secs();
            let mins = total_secs / 60;
            let secs = total_secs % 60;
            format!("{:02}:{:02}", mins, secs)
        }
        let instant_map = self.instant_map.clone();
        let guard = instant_map.read().map_err(|err| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!(
                    "Failed to get write lock of uuid {}, error: {}",
                    args.instance_id, err
                ),
                None,
            )
        })?;
        guard
            .get(&args.instance_id)
            .map(|info| {
                CallToolResult::success(vec![
                    Content::text(format!("instance label :{}", info.label)),
                    Content::text(format!(
                        "time has elpased  {}",
                        format_duration_mm_ss(info.start_instance.elapsed())
                    )),
                ])
            })
            .ok_or(McpError::new(
                ErrorCode::INVALID_PARAMS,
                format!(
                    "not found anything through and isntance id {}",
                    args.instance_id
                ),
                None,
            ))
    }
    #[tool(description = "通过绝对路径获取文件的内容")]
    async fn read_file(
        &self,
        Parameters(args): Parameters<ReadFileArgs>,
    ) -> Result<CallToolResult, McpError> {
        let file_bytes = fs::read(&args.file_path).await.map_err(|err| {
            McpError::new(
                ErrorCode::RESOURCE_NOT_FOUND,
                format!(
                    "file {} is not found, ask for the right file path, error:{}",
                    args.file_path, err
                ),
                None,
            )
        })?;
        let text = String::from_utf8(file_bytes).map_err(|_| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                "failed to parse the text which contains invalid character",
                None,
            )
        })?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
    #[tool(description = "使用绝对路径创建一个文件，并且写入内容")]
    async fn create_file(
        &self,
        Parameters(args): Parameters<CreateFileArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut file = fs::File::create(&args.file_path).await.map_err(|err| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!(
                    "Failed to create file in {}, error: {}",
                    &args.file_path, err
                ),
                None,
            )
        })?;
        if file.write_all(args.context.as_bytes()).await.is_ok() {
            return Ok(CallToolResult::success(vec![]));
        }
        Err(McpError::new(
            ErrorCode::INTERNAL_ERROR,
            "failed to write file",
            None,
        ))
    }
    #[tool(description = "在服务终端执行一段命令，返回调用命令的结果")]
    async fn use_cmd(
        &self,
        Parameters(args): Parameters<CmdArgs>,
    ) -> Result<CallToolResult, McpError> {
        
        let output = Command::new("sh")
            .arg("-c")
            .arg(&args.cmd)
            .output()
            .await
            .map_err(|err| {
                McpError::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("invalid invoke cmd {}, error: {}", args.cmd, err),
                    None,
                )
            })?;
        if output.status.success() {
            let out = String::from_utf8(output.stdout).unwrap_or(String::new());
            Ok(CallToolResult::success(vec![Content::text(out)]))
        } else {
            let err_out = String::from_utf8(output.stderr).unwrap_or(String::new());
            Err(McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("error in excuting cmd  : {}", err_out),
                None,
            ))
        }
    }
    #[tool(description = "通过网络通过Get方法访问url，并且返回内容")]
    async fn get_url(
        &self,
        Parameters(args): Parameters<GetUrlArgs>,
    ) -> Result<CallToolResult, McpError> {
        let text = reqwest::get(&args.url)
            .await
            .map_err(|err| {
                McpError::new(
                    ErrorCode::INVALID_REQUEST,
                    format!("Failed to get url :{}, error: {}", args.url, err),
                    None,
                )
            })?
            .text()
            .await
            .map_err(|_| {
                McpError::new(
                    ErrorCode::INVALID_REQUEST,
                    format!("the repsonse is not String"),
                    None,
                )
            })?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

#[prompt_router]
impl InterviewTool {
    /// how to countd down
    #[prompt(name = "实现计时器的方法")]
    async fn counter(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<Vec<PromptMessage>, McpError> {
        Ok(vec![PromptMessage::new_text(
            rmcp::model::PromptMessageRole::Assistant,
            "如果想要对回答倒计时，通过 current_instant 记录下提问的时间， 等到用户回答的时候再使用 elapsed_since 计算花了多少时间回答问题",
        )])
    }

    #[prompt(name = "寻找系统默认目录，默认文档(documents)，默认下载(downloads)等")]
    async fn default_directory(
        &self,
        Parameters(args): Parameters<DirInfo>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<Vec<PromptMessage>, McpError> {
        let path = match args.name {
            DirName::Downloads => "~/Downloads",
            DirName::Documents => "~/Documents",
        };
        Ok(vec![PromptMessage::new_text(
            rmcp::model::PromptMessageRole::Assistant,
            path,
        )])
    }
}

#[tool_handler]
#[prompt_handler]
impl ServerHandler for InterviewTool {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Support tool for recording moments, ideal for time-limited interviews and tests"
                    .into(),
            ),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
            ..Default::default()
        }
    }
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![
                self._create_resource_text("str:////Users/to/some/path/", "cwd"),
                self._create_resource_text("memo://insights", "memo-name"),
            ],
            next_cursor: None,
        })
    }
}

//  mod interview_tool_test {
//    use std::env::home_dir;

//     use anyhow::{Context, Ok};
//     use rmcp::handler::server::wrapper::Parameters;

//     use crate::interview_tool::InterviewTool;
//     #[tokio::test]
//     async fn test_use_cmd() -> anyhow::Result<()> {
//         let tool = InterviewTool::new();
//         tool.use_cmd(Parameters(super::CmdArgs {
//             cmd: String::from("ls ~/Downloads/*.mp4"),
//         }))
//         .await?;
//         anyhow::Ok(())
//     }
//     #[tokio::test]
//     async fn find_home_dir() -> anyhow::Result<()> {
//         let home_dir = home_dir().context("not find")?;
//         println!("home dir {}",home_dir.to_str().unwrap());
//         Ok(())
//     } 
// }