//! 项目级全局变量集中定义。

use std::sync::atomic::AtomicU8;

/// 标识当前是否为用户手动终止。
/// 0 = 非手动终止；1 = 用户已点击终止。
pub(crate) static MANUAL_STOP: AtomicU8 = AtomicU8::new(0);
