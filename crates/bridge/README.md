# CLI 与凭据边界

所有 API 同步执行；桌面端必须放到串行工作线程，不能阻塞 GPUI。

## 公共接口

- `Credentials`：四个公开 `String` 字段 `lark_app_id`、`lark_app_secret`、`lark_user_token`、`tapd_token`。支持 `Clone` / `Default`；`Debug` 隐去全部字段，析构时清零。因为实现 `Drop`，不要使用结构更新语法 `..Default::default()`。
- `load_credentials() -> Result<Credentials>`：仅从 macOS 原生 Keychain 读取；无记录时返回空凭据。
- `save_credentials(&Credentials) -> Result<()>`：非空输入覆盖相应旧字段，空输入保留旧值；一次写入一个加密 Keychain 项。已有 App ID 与新值不同时拒绝保存，必须先明确清除，避免串用旧密钥。保存成功后应重新加载完整凭据，不能直接使用稀疏输入作为运行时账户。
- `clear_credentials() -> Result<()>`：显式删除上述 Keychain 项，原本不存在也成功；用于换账户或从手动令牌切换到 CLI OAuth。调用方必须先取得用户确认。
- `credentials_namespace(&Credentials) -> String`：带长度分隔的 SHA-256 凭据指纹；包含全部字段，但不再参与账户数据库命名空间。不能把它当作已验证的账户标识。
- `CliGateway::new(Settings, Credentials) -> Result<CliGateway>`：要求两条 CLI 路径均为绝对路径，TAPD endpoint 仅允许 `https://api.tapd.cn`。
- `check_lark()` / `check_tapd() -> Result<()>`：只读连接检查；TAPD 先执行 `user info`，再按配置 workspace 执行 `story list`，不依赖新版项目可能没有的旧 task 读取权限。此检查不证明写权限。
- `login_lark() -> Result<()>`：仅用户点击时执行 `auth login --scope task:task:read --json`，最多 180 秒，然后检查状态。不启动自动登录，不执行配置导出。
- `oauth_status() -> Result<AuthStatus>`：执行 `auth status --json --verify`；返回 `authenticated: bool` 与 `user_id: String`，不向 UI 暴露原始认证响应。
- `account_namespace() -> Result<String>`：要求 Lark 状态 `identity=user`、`verified=true`，公开 `brand` / `appId` / `identities.user.openId` 均有效；再通过 `tapd-cli user info` 的 `data.id` 验证实际 TAPD Token 主体。稳定键仅包含版本、语义提供商标识和以上公开身份，同一账户轮换令牌或搬迁 CLI 不改变键。手动注入 Lark user token 一律拒绝，提示清除凭据后使用 CLI OAuth。桌面端应在操作前重新计算并阻止跨账户复用草稿。
- 实现 `lark2tapd_core::Gateway` 全部方法。`validate_draft` 本地验证数字 workspace、标题和日期，供核心在不确定日志之前调用；`get_source` 只读选中的单个任务。写入成功后根据实际返回的 `Task` / `Story` 类型读回并核对 title / description / owner / due；读回失败或任何字段不符仍返回错误，由核心保留不确定结果，不自动重试写入。
- `safe_url(raw, lark) -> String`：仅保留 HTTPS 飞书 / Lark 或 TAPD 域名 URL；无可靠 URL 时返回空字符串。

## 安全与边界

进程直接通过 `Command` 启动，不使用 shell。凭据只进环境变量；任务内容按 CLI 契约进入 argv（TAPD 不支持 stdin），因此任务内容可能在本机进程参数中可见，绝不放入日志或错误。运行环境清除后只保留 HOME、必要本地化及证书位置等白名单，PATH 保留 GUI 原始值并补充可执行文件目录和常见 Node 安装目录。

单次普通调用 30 秒；每个输出管道最多缓存 4 MiB。超时、失败、超限都会终止进程组并回收直接子进程；非阻塞管道读写共用截止时间，即使后代 `setsid` 后保留管道也不会无限等待。错误不包含 argv、stdout、stderr。假 CLI 测试在临时目录运行，不读写用户真实认证或业务数据。

飞书 `my_tasks` 按原生命令的 owned/assigned 语义获取所有状态；逐页读取并逐项获取完整详情，整个刷新最多 120 秒、1000 个任务，分页缺失、重复、非法 GUID 或超限直接失败，不返回半份任务。全天和定时任务的毫秒时间戳均按本机时区转换为 `YYYY-MM-DD`，当前同步模型仅保存日期，明确丢弃时分，不能保留作者原始时区。

描述转义所有 HTML 特殊字符，再追加可见文本 `[lark-task:{guid}]` 及通过白名单验证的来源 URL。返回的远端 HTML 作为冲突基线，不作为本地可执行 HTML 渲染。

## 身份协议证据与未验证边界

- `lark-cli whoami --help` 只承诺当前有效身份诊断；官方 [whoami 源码](https://github.com/larksuite/cli/blob/main/cmd/whoami/whoami.go) 对内建凭据明确为 local-only，并传递 `verify=false`，不能验证注入令牌的主体。因此不猜测不存在的 authen 服务，而是拒绝此配置。
- OAuth 状态字段依据官方 [auth/status.go](https://github.com/larksuite/cli/blob/main/cmd/auth/status.go)：`appId`、`brand`、`identity`、`identities.user.openId`，以及 `--verify` 后有效身份的 `verified`。
- TAPD 已安装 `tapd-cli user info --help` 明确为 `GET /users/info`；[官方接口文档](https://open.tapd.cn/document/api-doc/API文档/api_reference/user/get_users_info.html) 返回 `data.id` 作为公开用户唯一标识，仅支持用户态 OAuth Token。无法验证的其他认证形式拒绝进入同步。
- 未运行真实账户 API、OAuth 或 Keychain 读写/删除；Keychain 仅编译验证。清空日期是否被服务端接受仍需授权环境验证，忽略清空时会保留不确定状态。
- 描述采用严格读回相等策略。TAPD 若规范化 HTML（即使语义相同），仍会安全地报不确定，需人工核对；未用可能掩盖数据丢失的 HTML 去标签比较。
- 新版稳定键与旧版凭据哈希键不同。旧库尤其不确定写入不能自动丢弃或猜测迁移，调用方必须保留并安排显式核对；此桥接层不执行历史库迁移。
