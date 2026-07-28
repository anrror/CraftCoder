# CraftCoder Eval 基准报告

> 生成日期: 2026-07-27
> 环境: Windows x86_64, Python 3.13, Rust 1.80+, 无 Docker

## 1. 测试总结

| 指标 | 结果 |
|------|------|
| **总任务数** | 12 |
| **通过** | 12 |
| **失败** | 0 |
| **通过率** | **100.0%** |
| **平均延迟** | 1372ms |
| **测试框架** | 本地无 Docker HumanEval 风格基准 |

## 2. 逐任务结果

| 任务 | 类别 | 状态 | 延迟 | 描述 |
|------|------|------|------|------|
| HumanEval/0 | 基础运算 | ✅ pass | 1185ms | 基本算术断言 |
| HumanEval/1 | 函数定义 | ✅ pass | 1224ms | add 函数 + 多断言 |
| HumanEval/2 | 递归 | ✅ pass | 1334ms | 斐波那契递归 |
| HumanEval/3 | 字符串 | ✅ pass | 1337ms | 字符串反转 (chr ord) |
| HumanEval/4 | 布尔逻辑 | ✅ pass | 1336ms | is_even 奇偶判断 |
| HumanEval/5 | 集合操作 | ✅ pass | 1218ms | Counter 字符统计 |
| HumanEval/6 | 排序合并 | ✅ pass | 1669ms | 归并两个有序数组 |
| HumanEval/7 | 数学 | ✅ pass | 1404ms | 缺失数字查找(高斯) |
| HumanEval/8 | 数论 | ✅ pass | 1266ms | GCD 最大公约数 |
| HumanEval/9 | 搜索 | ✅ pass | 1339ms | 二分搜索 |
| HumanEval/10 | 素数 | ✅ pass | 1547ms | 素数判定 |
| HumanEval/11 | 输出验证 | ✅ pass | 1405ms | sum(range) 预期输出 |

## 3. Rust Eval 框架测试

| 测试套件 | 结果 |
|----------|------|
| `code-agent-eval` 类型测试 | ✅ 3/3 通过 |
| `code-agent-eval` 适配器测试 | ✅ 通过 |
| `code-agent-eval` 指标测试 | ✅ 通过 |
| `eval-runner` 集成测试 | ✅ 通过 |

## 4. Python Eval 套件测试

| 测试套件 | 结果 |
|----------|------|
| `test_models_roundtrip.py` | ✅ 7/7 通过 |
| EvalTask 序列化 | ✅ |
| EvalResult 序列化 | ✅ |
| EvalStatus 枚举 | ✅ |
| Frozen Model 强制 | ✅ |

## 5. 代码质量基线

| 指标 | 值 |
|------|-----|
| 核心单元测试 | **629 passed** |
| Web 测试 | **12 passed** |
| Clippy Lint | **零错误** |
| 工作区编译 | **通过** |

## 6. 已知限制

- Docker 不可用 — 沙箱隔离测试跳过
- 本地基准仅覆盖 Python 函数级算法任务
- 未运行 SWE-bench-Live（需要 Docker + Git 环境）
- 未测试真实 LLM 调用（需要 API Key）

## 7. 后续建议

1. 在 Docker 环境中运行完整 SWE-bench-Live
2. 配置 LLM API Key 后运行端到端 Eval
3. 扩展本地基准至 50+ HumanEval 任务
4. 集成 CI 自动运行基准
