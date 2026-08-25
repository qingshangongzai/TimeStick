# TimeStick（时间贴）— AI 协作入口

全屏时间悬浮窗：纯 Rust + Win32（Direct2D/DirectWrite）单 exe 桌面常驻工具，仅 Windows 平台。

## 规则（动手前先读）

- **编码规则唯一真源**：`开发规范.md` —— 开始任何编码 / 审查 / 提交 / 写方案任务前，先读它；任何 skill 的规则与它冲突时，以它为准。

## Skill 触发对照（用户说关键词时自动用）

| 用户意图 | Skill | 触发词 |
|---|---|---|
| 审查代码 | `code-review` | 检查代码、代码审查、review、核对修改 |
| 生成提交信息 | `commit-message-generator` | 提交信息、commit |
| 创建分支 | `create-branch` | 创建分支、开个分支、分支名 |
| 写/更新方案文档 | `plan-doc` | 写方案、实施计划、落地记录 |
| 只分析不修改 | `question-mode` | 提问、提问模式 |

## 约定

- skill 以 `skills/`（已提交仓库）为唯一维护源；各工具目录（`.trae/skills/`、`.zcode/skills/`）下的副本从它同步（`Copy-Item -Path skills\* -Destination .trae\skills -Recurse -Force`，zcode 同理），永不直接修改。
- 版本号与依赖版本唯一来源：根目录 `Cargo.toml`（禁止手改任何其他处的版本号）。
