# WebView 渲染性能规范

适用于所有使用 Tauri + WebView 渲染的前端代码。

## 1. 轮询频率

**禁止**使用固定高频轮询驱动 UI 更新。

| 场景 | 允许频率 |
|------|---------|
| 有活跃任务时 | ≤ 500ms |
| 无活跃任务时 | ≥ 2000ms |
| 静态数据（设备列表等） | ≥ 2000ms |

**必须**根据业务状态动态调整轮询频率，而不是用同一个 interval 覆盖所有场景。

## 2. setState 调用

**禁止**在数据未发生变化时调用 setState。每次轮询拿到新数据后，必须与上次结果比较，相同则跳过更新。

```tsx
const prevRef = useRef<string>('')

const fetch = async () => {
  const result = await invoke(...)
  const key = JSON.stringify(result)
  if (key === prevRef.current) return
  prevRef.current = key
  setState(result)
}
```

原因：setState → React re-render → WebView repaint → GPU，即使页面视觉上没有变化也会消耗 GPU。

## 3. CSS transition / animation

**禁止**在高频更新的属性上使用 `transition`。

高频更新的属性包括：进度条宽度（`width`）、数值文本、速度/ETA 显示等。

```css
/* 禁止 */
.progress-fill {
  transition: width 0.3s ease;
}
```

`transition` 在每次属性变化时触发 GPU 合成层动画。若该属性每 500ms 变化一次，则 GPU 持续处于工作状态。

`transition` 只允许用于用户交互触发的状态变化（hover、focus、点击），不允许用于数据驱动的属性变化。

## 4. 根因总结

WebView GPU 占用高的根本原因链：

```
高频 setInterval
  → 无条件 setState
    → React re-render
      → WebView repaint
        → GPU 持续工作
```

打断这条链上的任意一环均可降低 GPU 占用，优先级从高到低：

1. 降低轮询频率（最大收益）
2. 数据不变时跳过 setState
3. 移除高频属性的 CSS transition
