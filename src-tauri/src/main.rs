// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// v0.0.2.2 关键修复：在 main.rs 中调用 `tauri::generate_context!()` 宏。
//
// **为什么必须在这里**：
// 该宏会在编译时展开成大量代码（读 tauri.conf.json + 嵌入所有静态资源 +
// 生成所有 command 注册 trait impl），体积超过 12GB。如果在 lib.rs 中调用，
// rustc 在生成 bound_launch_lib.rlib 时会触发 E0786 (corrupt metadata) 错误
// （这是 rustc 1.96.1 处理大 rlib 的已知 bug）。
//
// 把宏调用移到 main.rs（bin 编译阶段）后：
// - lib.rs 只接收一个普通的 `tauri::Context` 参数，不再含宏生成代码
// - bound_launch_lib.rlib 体积大幅缩小，不再触发 E0786
// - bin 编译阶段 rustc 处理宏展开是稳定路径（社区标准做法）
fn main() {
    bound_launch_lib::run_with_context(tauri::generate_context!());
}
