//! 集成测试公共件:本地 JSON mock server 已抽到 lib([`wanning_demo::mock_server`])
//! ——场景运行时(`--scenario full-loop-mock`,W-29)与集成测试共用同一份实现,
//! 不再各自维护。本文件只保留再导出,既有测试的 `common::spawn_json_mock(...)`
//! 引用路径不变。

#![allow(unused_imports)] // 各测试二进制只用其中一部分(与原 dead_code 同理)

pub use wanning_demo::mock_server::{spawn_json_mock, MockJsonServer, MockResponse};
