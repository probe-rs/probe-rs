# probe-rs 后端改进方案

## 概述
本文档描述了为 probe-rs 调试后端实现的改进，以支持 VS Code 扩展的 Live Watch 动态变量修改功能。

## 改进内容

### 1. setVariable 请求处理模块 (`set_variable.rs`)
**位置**: `probe-rs-tools/bin/probe-rs/cmd/dap_server/debug_adapter/dap/set_variable.rs`

**功能**:
- 实现 DAP `setVariable` 请求的处理
- 支持运行时变量修改（无需暂停调试器）
- 管理待修改变量队列

**关键组件**:
```rust
pub struct PendingVariableModification {
    pub variable_key: ObjectRef,
    pub new_value: String,
    pub variable_name: String,
}

pub struct VariableModificationQueue {
    modifications: HashMap<String, PendingVariableModification>,
}
```

**工作流程**:
1. 前端发送 `setVariable` 请求
2. 检查调试器状态
3. 如果运行中：将修改加入队列，返回 "Pending" 状态
4. 如果暂停：立即应用修改，返回新值

### 2. 变量修改管理器 (`variable_modifier.rs`)
**位置**: `probe-rs-tools/bin/probe-rs/cmd/dap_server/server/variable_modifier.rs`

**功能**:
- 管理待修改和已应用的变量修改
- 批量应用待修改项
- 维护修改历史记录

**关键组件**:
```rust
pub struct VariableModifier {
    pending_modifications: HashMap<String, PendingModification>,
    applied_modifications: HashMap<String, AppliedModification>,
}
```

**主要方法**:
- `queue_modification()` - 将修改加入队列
- `apply_modification()` - 应用单个修改
- `apply_all_modifications()` - 批量应用所有待修改项
- `get_pending()` - 获取所有待修改项
- `get_history()` - 获取修改历史

## 集成指南

### 1. 在 DAP 适配器中集成 setVariable 处理

在 `debug_adapter/dap/adapter.rs` 中添加：

```rust
use crate::cmd::dap_server::debug_adapter::dap::set_variable;

// 在请求处理中
"setVariable" => {
    let variable_ref = request.arguments.get("variablesReference")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let name = request.arguments.get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let value = request.arguments.get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    match set_variable::handle_set_variable(
        &mut self.debugger_state,
        variable_ref,
        name,
        value,
    ) {
        Ok(response) => {
            // 发送响应
        }
        Err(e) => {
            // 发送错误
        }
    }
}
```

### 2. 在调试器状态中集成修改管理器

在 `server/debugger.rs` 中添加：

```rust
use crate::cmd::dap_server::server::variable_modifier::VariableModifier;

pub struct DebuggerState {
    // ... 现有字段
    variable_modifier: VariableModifier,
}

impl DebuggerState {
    pub fn queue_variable_modification(
        &mut self,
        variable_key: ObjectRef,
        name: String,
        value: String,
    ) {
        self.variable_modifier.queue_modification(variable_key, name, value);
    }

    pub fn apply_pending_modifications(
        &mut self,
        memory: &mut dyn MemoryInterface,
        variable_cache: &mut VariableCache,
    ) -> Result<Vec<String>, Vec<String>> {
        self.variable_modifier.apply_all_modifications(memory, variable_cache)
    }
}
```

### 3. 在调试器暂停时应用待修改项

在 `server/debugger.rs` 的暂停处理中：

```rust
fn on_debugger_paused(&mut self) {
    // ... 现有代码
    
    // 应用所有待修改的变量
    if let Err(errors) = self.apply_pending_modifications(&mut memory, &mut variable_cache) {
        for error in errors {
            tracing::warn!("Failed to apply pending modification: {}", error);
        }
    }
}
```

### 4. 在模块中导出新模块

在 `debug_adapter/dap/mod.rs` 中添加：
```rust
pub mod set_variable;
```

在 `server/mod.rs` 中添加：
```rust
pub mod variable_modifier;
```

## 工作流程示例

### 场景 1：调试器运行时修改变量

```
前端                          后端
  |                            |
  |-- setVariable(x, 42) ----->|
  |                            |
  |<-- Pending: 42 ------------|
  |                            |
  |-- continue() ------------->|
  |                            |
  |<-- stopped (breakpoint) ---|
  |                            |
  |-- apply_pending() -------->|
  |                            |
  |<-- x = 42 (applied) -------|
```

### 场景 2：调试器暂停时修改变量

```
前端                          后端
  |                            |
  |-- setVariable(x, 42) ----->|
  |                            |
  |<-- x = 42 (applied) -------|
```

## 性能考虑

1. **内存效率**：使用 HashMap 存储待修改项，O(1) 查询时间
2. **批量应用**：在调试器暂停时批量应用所有待修改项，减少内存访问次数
3. **历史记录**：可选的修改历史记录，用于调试和审计

## 错误处理

- 变量不存在：返回错误信息
- 内存写入失败：返回详细的错误原因
- 类型不匹配：由 `Variable::update_value()` 处理
- 部分失败：记录失败项，继续处理其他项

## 测试

两个模块都包含单元测试：

```bash
# 运行测试
cargo test -p probe-rs-tools set_variable
cargo test -p probe-rs-tools variable_modifier
```

## 未来改进

1. **条件修改**：支持条件表达式（如 `x > 10 ? 42 : x`）
2. **批量修改**：支持一次修改多个变量
3. **修改验证**：在应用前验证新值的有效性
4. **修改撤销**：支持撤销已应用的修改
5. **修改通知**：向前端发送修改完成通知

## 相关文件

- 前端改进文档：`../../vscode/LIVE_WATCH_IMPROVEMENTS.md`
- probe-rs-debug 文档：`probe-rs-debug/README.md`
- DAP 规范：https://microsoft.github.io/debug-adapter-protocol/

## 贡献指南

在修改这些模块时，请确保：
1. 添加适当的单元测试
2. 更新相关文档
3. 遵循现有的代码风格
4. 运行 `cargo fmt` 和 `cargo clippy`
