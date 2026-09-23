/**
 * 图表与状态配色常量。
 *
 * 集中定义，避免各页面重复硬编码颜色 —— 也方便后续统一调色。
 * 深浅色主题下都用这一套（当前控制台为暗色，颜色已按暗背景挑选对比度）。
 */

/** 指标序列配色（CPU/GPU/NPU/内存/存储/温度/网络） */
export const METRIC_COLORS = {
  cpu: '#4f9cf9',
  gpu: '#a76bf9',
  npu: '#00d4a0',
  mem: '#faad14',
  storage: '#13c2c2',
  temp: '#ff7875',
  netRx: '#52c41a',
  netTx: '#eb2f96',
} as const

/** 告警级别配色 */
export const ALERT_COLORS = {
  warning: '#faad14',
  critical: '#ff4d4f',
} as const

/** ECharts 通用暗色主题片段 */
export const CHART_THEME = {
  text: '#8c8c8c',
  textStrong: '#e6edf3',
  axisLine: '#2a3444',
  splitLine: '#222b38',
  tooltipBg: '#1a212b',
  tooltipBorder: '#2a3444',
} as const

/** 字体：等宽用于数值/ID，保证列对齐不跳动 */
export const MONO_FONT = "'JetBrains Mono', 'Cascadia Mono', Consolas, monospace"

/**
 * 把图表数据里的 null 转成 ECharts 的断点。
 *
 * 关键：拿不到的指标必须是**断点**，不能补 0 —— 补 0 就是把"没数据"
 * 伪装成"数值为 0"，属于造假。配合 `connectNulls: false` 生效。
 */
export function withGaps(values: (number | null | undefined)[]): (number | null)[] {
  return values.map((v) => (v === null || v === undefined || Number.isNaN(v) ? null : Number(v)))
}
