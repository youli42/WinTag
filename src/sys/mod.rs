pub mod badge;
pub mod overlay;
pub mod win_event;
pub mod window;

/// 类型别名重导出：`TagStore`（`HashMap<isize, Tag>`）
///
/// overlay 层的 `set_tag_store` 签名需要引用该类型，但依赖方向约定
/// `ui → core → sys` 禁止 sys 层以 `crate::` 路径源码级引用 core。
/// 此处以相对路径重导出该类型别名，仅供编译期类型解析使用，
/// 不产生任何运行时（行为）依赖。
pub(crate) use super::core::tag::TagStore;
