//! 跨二进制共享的底座。
//!
//! 各二进制按需引入，两种方式：
//!
//! - **工具/界面类二进制**（主控台 / 安装器 / 更新器）：
//!   `#[path = "../shared/mod.rs"] mod shared;` 整体引入 —— 每个二进制只取用
//!   其中一部分是常态，用不到的条目由下面的 `allow(dead_code)` 豁免噪音。
//! - **后台服务**：逐文件 `#[path = "../shared/cache.rs"] pub(crate) mod cache;`
//!   单独引入 —— 服务端刻意只编译它真正需要的两个文件（缓存探测与机主昵称），
//!   不碰账号凭据与进程管理。
//!
//! 两条约定：
//!
//! 1. **共享文件之间不要互相引用**：各文件零交叉依赖，才能被单独按需引入
//!    （`font.rs`、`sha256.rs` 就是单独引入的例子 —— 它们分别带 GUI 依赖
//!    和只被两个二进制使用，放进来会牵连其它二进制）。
//! 2. **每个条目至少要被一个二进制真实使用**：下面的整体豁免只是为了压掉
//!    "这个二进制没用到那个条目"的噪音（每个二进制只取用一部分是常态），
//!    不是给死代码开的后门 —— 加新条目前先确认有人用。

#![allow(dead_code)]

pub mod cache;
pub mod credentials;
pub mod dialog;
pub mod paths;
pub mod preflight;
pub mod process;
pub mod self_name;
pub mod session;
