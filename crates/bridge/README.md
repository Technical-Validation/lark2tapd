# CLI 与凭据边界

所有 API 同步执行；桌面端必须放到串行工作线程，不能阻塞 GPUI。

## 公共接口

- `Credentials`：四个公开 `String` 字段 `lark_app_id`、`lark_app_secret`、`lark_user_token`、`tapd_token`。支持 `Clone` / `Default`；`Debug` 隐去全部字段，析构时清零。因为实现 `Drop`，不要使用结构更新语法 `..Default::default()`。
- `load_credentials() -> Result<Credentials>`：仅从 macOS 原生 Keychain 读取；无记录时返回空凭据。
- `save_credentials(&Credentials) -> Result<()>`：非空输入覆盖相应旧字段，空输入保留旧值；一次写入一个加密 Keychain 项。已有 App ID 与新值不同时拒绝保存，必须先明确清除，避免串用旧密钥。保存成功后应重新加载完整凭据，不能直接使用稀疏输入作为运行时账户。
- `clear_credentials() -> Result<()>`：显式删除上述 Keychain 项，原本不存在也成功；用于换账户或从手动令牌切换到 CLI OAuth。调用方必须先取得用户确认。
- `credentials_namespace(&Credentials) -> String`：带长度分隔的 SHA-256 凭据指纹；包含全部字段，但不再参与账户数据库命名空间。不能把它当作已验证的账户标识。
- `CliGateway::new(Settings, Credentials) -> Result<CliGateway>`：构造不联网；每次调用只校验该平台的绝对 CLI 路径及凭据。TAPD 调用另校验 endpoint 仅为 `https://api.tapd.cn`，因此可独立诊断其中一个平台而不依赖另一个平台已配置。
- `check_lark()` / `check_tapd() -> Result<()>`：只读连接检查；飞书先验证 OAuth 用户身份，再读取一页相关任务验证任务权限，不访问 TAPD 可执行文件。TAPD 先执行 `user info`；仅当 workspace 非空时执行 `story list`，因此首次认证不要求已选项目。此检查不证明写权限。
- `list_workspaces() -> Result<Vec<TapdWorkspace>>`：执行 `user workspaces`，严格读取 `data: [{Workspace: {id, name}}]`；只列管理员项目，**不是所有可访问项目**。ID 必须为数字字符串，名称必须非空；不支持的结构直接报错，不回退为空列表。
- `list_members(workspace) -> Result<Vec<TapdMember>>`：执行 `user members workspaceid=<id> fields=user,name`，读取未分页的 `data: [{UserWorkspace: {user, name}}]`。`account` 保存 `user` 昵称账号，不使用数字用户 ID；缺账号或有意义的显示名时报错。
- `verify_workspace(id) -> Result<()>`：通过上述成员接口验证显式输入的项目 ID。普通成员可由 UI 手动输入项目 ID/名称，不需要管理员项目枚举成功。目录缓存及按已验证账户隔离由核心/调用方负责；这些方法不会自动联网。
- `login_lark() -> Result<()>`：仅用户点击时执行 `auth login --scope task:task:read --json`，最多 180 秒，然后检查状态。不启动自动登录，不执行配置导出。
- `oauth_status() -> Result<AuthStatus>`：执行 `auth status --json --verify`；返回 `authenticated: bool` 与 `user_id: String`，不向 UI 暴露原始认证响应。
- `account_namespace() -> Result<String>`：要求 Lark 状态 `identity=user`、`verified=true`，公开 `brand` / `appId` / `identities.user.openId` 均有效；再通过 `tapd-cli user info` 的 `data.id` 验证实际 TAPD Token 主体。稳定键仅包含版本、语义提供商标识和以上公开身份，同一账户轮换令牌或搬迁 CLI 不改变键。手动注入 Lark user token 一律拒绝，提示清除凭据后使用 CLI OAuth。桌面端应在操作前重新计算并阻止跨账户复用草稿。
- 实现 `lark2tapd_core::Gateway` 全部方法。`validate_draft` 本地验证数字 workspace、标题和日期，供核心在不确定日志之前调用；`get_source` 重新验证 OAuth 主体，只读选中的单个原生任务，并再次确认创建者或负责人包含本人；关系已移除则拒绝提交。写入成功后根据实际返回的 `Task` / `Story` 类型读回并核对 title / description / owner / due；读回失败或任何字段不符仍返回错误，由核心保留不确定结果，不自动重试写入。
- `safe_url(raw, lark) -> String`：仅保留 HTTPS 飞书 / Lark 或 TAPD 域名 URL；无可靠 URL 时返回空字符串。

## 应用内安装接口

`installer::installation_plan(tool, tools_dir)` 只生成来源/版本/私有目录计划，`install_cli(tool, tools_dir)` 必须在用户确认后由工作线程调用，返回验证过的绝对可执行路径；`probe_cli(tool, path)` 只校验版本与 CLI 标识，不代表远端认证通过。TAPD 自己的版本命令可能检查更新，不应称为完全无副作用。

飞书使用固定官方 npm 版本和已核实的二进制安装入口，TAPD 直接下载固定 CNB 官方发布物；不执行 TAPD shell 安装脚本。私有路径拒绝软链/遍历/覆盖已有目录，不使用 sudo，不修改 shell 配置。TAPD 没有官方加密校验值，来源信任边界在确认界面明确披露。

## 安全与边界

进程直接通过 `Command` 启动，不使用 shell。凭据只进环境变量；任务内容按 CLI 契约进入 argv（TAPD 不支持 stdin），因此任务内容可能在本机进程参数中可见，绝不放入日志或错误。运行环境清除后只保留 HOME、必要本地化及证书位置等白名单，PATH 保留 GUI 原始值并补充可执行文件目录和常见 Node 安装目录。

单次普通调用 30 秒；每个输出管道最多缓存 4 MiB。超时、失败、超限都会终止进程组并回收直接子进程；非阻塞管道读写共用截止时间，即使后代 `setsid` 后保留管道也不会无限等待。错误不包含 argv、stdout、stderr。假 CLI 测试在临时目录运行，不读写用户真实认证或业务数据。

飞书任务刷新每次先调用一次 `auth status --json --verify`，只接受已验证的用户 `openId`。使用 `task +get-related-tasks --as user --format json --page-limit 1`，通过 `--page-token` 手动继续读取 `data.items` / `has_more` / `page_token`。`my_tasks` 会漏掉仅由本人创建的任务，因此不作为候选来源。仅保留创建者 ID 为本人或 `role=assignee` 成员 ID 包含本人的候选；只有关注关系、其他人、缺失身份或仅同名均不算。候选在详情读取前按 GUID 去重；原生 `task tasks get` 返回后再验证关系，已移除的任务不进入结果。

认证、所有分页及原生详情共用 120 秒截止时间，最多 40 页、1000 条候选列表项（包括过滤或重复项）。缺失/重复游标、非法 GUID、任务详情失败或超限均整体失败，不返回半份任务。每次原生详情提供创建者和负责人 ID/姓名；未知姓名保留空字符串，不增加通讯录请求。`SourceMetadata` 保留原生 `created_at` / `completed_at` / `due.timestamp` 毫秒字符串，包括完成时间的 `"0"`。快捷命令会把创建/完成时间变成本地格式日期，绝不把这些字符串保存为原始毫秒。

用于 TAPD 的 `due` 仍按本机时区转换为 `YYYY-MM-DD`，丢弃时分；原始 `metadata.due_at` 独立保留。不能据此恢复任务作者原始时区。

描述转义所有 HTML 特殊字符，再追加可见文本 `[lark-task:{guid}]` 及通过白名单验证的来源 URL。返回的远端 HTML 作为冲突基线，不作为本地可执行 HTML 渲染。

## 身份协议证据与未验证边界

- `lark-cli whoami --help` 只承诺当前有效身份诊断；官方 [whoami 源码](https://github.com/larksuite/cli/blob/main/cmd/whoami/whoami.go) 对内建凭据明确为 local-only，并传递 `verify=false`，不能验证注入令牌的主体。因此不猜测不存在的 authen 服务，而是拒绝此配置。
- OAuth 状态字段依据官方 [auth/status.go](https://github.com/larksuite/cli/blob/main/cmd/auth/status.go)：`appId`、`brand`、`identity`、`identities.user.openId`，以及 `--verify` 后有效身份的 `verified`。
- TAPD 已安装 `tapd-cli user info --help` 明确为 `GET /users/info`；[官方接口文档](https://open.tapd.cn/document/api-doc/API文档/api_reference/user/get_users_info.html) 返回 `data.id` 作为公开用户唯一标识，仅支持用户态 OAuth Token。无法验证的其他认证形式拒绝进入同步。
- 飞书相关列表及日期格式依据官方 v1.0.96 [`task_get_related_tasks.go`](https://github.com/larksuite/cli/blob/v1.0.96/shortcuts/task/task_get_related_tasks.go) 和 [`task_query_helpers.go`](https://github.com/larksuite/cli/blob/v1.0.96/shortcuts/task/task_query_helpers.go)；成员字段以原生任务详情为准。
- TAPD 管理员列表的 `Workspace` 包装由父任务授权的只读 `user workspaces` 调用观察确认：`status=1`，数组元素含字符串 `id` / `name` / `pretty_name`。测试仅保留合成数据，不保存真实项目名或 ID。成员 `UserWorkspace.user/name` 结构依据[官方成员接口文档](https://open.tapd.cn/document/api-doc/API文档/api_reference/workspace/get_workspace_users.html)。不借用其他 workspace 接口的结构猜测。
- 本桥接实现和回归测试未调用真实账户 API、OAuth、Keychain 或安装器；全部调用边界由临时目录中的假可执行文件覆盖。Keychain 仅编译验证。父任务的上述只读结构观察不代表桥接层已经做过真实端到端同步。清空日期是否被服务端接受仍需授权环境验证，忽略清空时会保留不确定状态。
- 描述采用严格读回相等策略。TAPD 若规范化 HTML（即使语义相同），仍会安全地报不确定，需人工核对；未用可能掩盖数据丢失的 HTML 去标签比较。
- 新版稳定键与旧版凭据哈希键不同。旧库尤其不确定写入不能自动丢弃或猜测迁移，调用方必须保留并安排显式核对；此桥接层不执行历史库迁移。
