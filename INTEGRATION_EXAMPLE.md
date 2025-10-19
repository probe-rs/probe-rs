# Live Watch 变量修改集成示例

## 概述

本文档展示了如何在 VS Code 调试扩展中集成 probe-rs 后端的变量修改功能，实现像 Keil 那样的动态监视变量修改能力。

## 架构设计

### 前端（VS Code 扩展）
- **liveWatchManager.ts**: 管理 Live Watch 变量列表
- **liveWatchCommandService.ts**: 处理用户命令（修改变量值）
- **DAP 客户端**: 发送 `setVariable` 请求到调试适配器

### 后端（probe-rs）
- **set_variable.rs**: 处理 DAP `setVariable` 请求
- **variable_modifier.rs**: 管理变量修改队列和应用
- **debugger.rs**: 集成修改队列到调试器状态

## 工作流程

### 1. 用户在 VS Code 中修改变量值

```typescript
// 前端：liveWatchCommandService.ts
async modifyVariable(variableName: string, newValue: string) {
    const variable = this.findVariable(variableName);
    if (!variable) return;
    
    // 发送 setVariable 请求到 DAP 服务器
    const response = await this.debugSession.customRequest('setVariable', {
        variablesReference: variable.variablesReference,
        name: variableName,
        value: newValue
    });
    
    return response;
}
```

### 2. 后端处理 setVariable 请求

```rust
// 后端：set_variable.rs
pub fn handle_set_variable(
    debugger_state: &mut DebuggerState,
    variable_reference: i64,
    name: String,
    value: String,
) -> Result<SetVariableResponse, String> {
    let variable_key = probe_rs_debug::ObjectRef::from(variable_reference);
    
    // 检查调试器是否正在运行
    if debugger_state.is_running() {
        // 如果正在运行，将修改加入队列
        debugger_state.queue_variable_modification(variable_key, name.clone(), value.clone());
        
        Ok(SetVariableResponse {
            value: format!("Pending: {}", value),
            variable_type: Some("pending".to_string()),
            // ...
        })
    } else {
        // 如果已暂停，立即应用修改
        match debugger_state.apply_variable_modification(variable_key, &name, &value) {
            Ok(new_value) => Ok(SetVariableResponse {
                value: new_value,
                // ...
            }),
            Err(e) => Err(format!("Failed to set variable: {}", e)),
        }
    }
}
```

### 3. 变量修改队列管理

```rust
// 后端：variable_modifier.rs
pub struct VariableModifier {
    pending_modifications: HashMap<String, PendingModification>,
    applied_modifications: HashMap<String, AppliedModification>,
}

impl VariableModifier {
    // 队列修改
    pub fn queue_modification(&mut self, variable_key: ObjectRef, variable_name: String, new_value: String) {
        // 将修改添加到待处理队列
    }
    
    // 应用单个修改
    pub fn apply_modification(&mut self, variable_key: ObjectRef, memory: &mut dyn MemoryInterface, variable_cache: &mut VariableCache) -> Result<String, String> {
        // 从队列中取出修改并应用到内存
    }
    
    // 批量应用所有待处理修改
    pub fn apply_all_modifications(&mut self, memory: &mut dyn MemoryInterface, variable_cache: &mut VariableCache) -> Result<Vec<String>, Vec<String>> {
        // 应用所有待处理修改
    }
}
```

### 4. 调试器集成

```rust
// 后端：debugger.rs（伪代码）
pub struct DebuggerState {
    variable_modifier: VariableModifier,
    // ...
}

impl DebuggerState {
    // 队列变量修改
    pub fn queue_variable_modification(&mut self, variable_key: ObjectRef, name: String, value: String) {
        self.variable_modifier.queue_modification(variable_key, name, value);
    }
    
    // 应用变量修改
    pub fn apply_variable_modification(&mut self, variable_key: ObjectRef, name: &str, value: &str) -> Result<String, String> {
        // 立即应用修改
    }
    
    // 在调试器暂停时应用所有待处理修改
    pub fn on_debugger_paused(&mut self, memory: &mut dyn MemoryInterface, variable_cache: &mut VariableCache) {
        match self.variable_modifier.apply_all_modifications(memory, variable_cache) {
            Ok(results) => {
                for result in results {
                    println!("Applied: {}", result);
                }
            }
            Err(errors) => {
                for error in errors {
                    eprintln!("Error: {}", error);
                }
            }
        }
    }
}
```

## 支持的数据类型

根据 `probe-rs-debug/src/language/value.rs` 的实现，支持以下数据类型的修改：

- **布尔类型**: `bool`
- **字符类型**: `char`
- **字符串**: `String`
- **整数类型**: `i8`, `i16`, `i32`, `i64`, `i128`, `u8`, `u16`, `u32`, `u64`, `u128`
- **浮点类型**: `f32`, `f64`

## 错误处理

### 前端错误处理

```typescript
// 前端：liveWatchCommandService.ts
async modifyVariable(variableName: string, newValue: string) {
    try {
        const response = await this.debugSession.customRequest('setVariable', {
            variablesReference: variable.variablesReference,
            name: variableName,
            value: newValue
        });
        
        if (response.value.startsWith('Pending:')) {
            // 修改已加入队列，等待调试器暂停
            this.showNotification(`Variable modification queued: ${variableName}`);
        } else {
            // 修改已立即应用
            this.showNotification(`Variable modified: ${variableName} = ${response.value}`);
        }
    } catch (error) {
        this.showError(`Failed to modify variable: ${error.message}`);
    }
}
```

### 后端错误处理

```rust
// 后端：variable_modifier.rs
pub fn apply_all_modifications(&mut self, memory: &mut dyn MemoryInterface, variable_cache: &mut VariableCache) -> Result<Vec<String>, Vec<String>> {
    let mut results = Vec::new();
    let mut errors = Vec::new();
    
    for modification in pending {
        match self.apply_modification(modification.variable_key, memory, variable_cache) {
            Ok(new_value) => {
                results.push(format!("{} = {}", modification.variable_name, new_value));
            }
            Err(e) => {
                // 记录错误但继续处理其他修改
                errors.push(format!("{}: {}", modification.variable_name, e));
            }
        }
    }
    
    if errors.is_empty() {
        Ok(results)
    } else {
        Err(errors)
    }
}
```

## 性能考虑

1. **队列机制**: 避免在调试器运行时频繁修改内存，提高性能
2. **批量应用**: 在调试器暂停时批量应用所有待处理修改，减少内存访问次数
3. **历史记录**: 保存应用的修改历史，便于调试和审计

## 测试

### 单元测试

```rust
#[test]
fn test_modification_queue() {
    let mut queue = VariableModificationQueue::new();
    assert_eq!(queue.count(), 0);
    
    let var_key = probe_rs_debug::ObjectRef::from(1i64);
    queue.queue_modification(var_key, "test_var".to_string(), "42".to_string());
    
    assert_eq!(queue.count(), 1);
    
    let mods = queue.get_all();
    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].new_value, "42");
}
```

### 集成测试

1. 启动调试会话
2. 在 Live Watch 中添加变量
3. 修改变量值（调试器运行中）
4. 验证修改已加入队列
5. 暂停调试器
6. 验证修改已应用到内存
7. 恢复调试器
8. 验证变量值已更新

## 文件位置

- **前端**: `e:\Programing\probe_rs_vscode\vscode\src\treeViews\`
- **后端**: `e:\Programing\probe-rs-backend\probe-rs-tools\bin\probe-rs\cmd\dap_server\`
  - `debug_adapter/dap/set_variable.rs`: DAP setVariable 请求处理
  - `server/variable_modifier.rs`: 变量修改管理器

## 下一步

1. 在 DAP 适配器中集成 `set_variable` 模块
2. 在调试器状态中集成 `variable_modifier` 模块
3. 实现调试器暂停事件的处理，应用待处理修改
4. 添加前端 UI 来显示待处理修改的状态
5. 创建完整的集成测试

## 参考资源

- [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/)
- [probe-rs 文档](https://probe.rs/)
- [VS Code 调试扩展开发](https://code.visualstudio.com/api/extension-guides/debugger-extension)
