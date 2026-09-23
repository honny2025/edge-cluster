import ReactECharts from 'echarts-for-react'
import { Empty, Spin } from 'antd'
import { CHART_THEME, MONO_FONT, withGaps } from '../utils/theme'
import { formatClock, formatDateTime } from '../utils/format'

/**
 * 通用时序折线图。
 *
 * 两个关键约定：
 *
 * 1. **null 画断点，不补 0**。拿不到的指标（比如手机不暴露 NPU 使用率）
 *    必须显示为断线，绝不能补 0 假装"使用率是 0"——那是造假。
 *    靠 `connectNulls: false` + `withGaps()` 实现。
 *
 * 2. **刷新无闪烁**。实时曲线每几秒更新一次，因此关掉入场动画、
 *    用 merge 模式增量更新（`notMerge={false}`），并保证组件 key 稳定。
 *    若用 notMerge，每次刷新整图重绘，视觉上就是不停闪。
 */

export interface ChartSeries {
  name: string
  color: string
  data: (number | null | undefined)[]
  /** 是否填充面积（单序列图建议开启，多序列会互相遮挡） */
  area?: boolean
  /** Tooltip 里的单位后缀，如 '%'、'℃' */
  unit?: string
}

interface MetricChartProps {
  /** X 轴时间戳（后端返回的 naive UTC ISO 串） */
  timestamps: string[]
  series: ChartSeries[]
  height?: number
  /** 固定 Y 轴上限。百分比图传 100，避免轴范围随数据抖动 */
  yMax?: number
  /** Y 轴/Tooltip 数值格式化 */
  valueFormatter?: (v: number) => string
  loading?: boolean
  /** 自定义空状态文案 */
  emptyText?: string
  /** 是否显示图例 */
  showLegend?: boolean
}

export default function MetricChart({
  timestamps,
  series,
  height = 240,
  yMax,
  valueFormatter,
  loading,
  emptyText = '暂无数据',
  showLegend = true,
}: MetricChartProps) {
  // 唯一的判空依据：时间轴有没有点。指标全为 null 也照样画（显示断点）
  if (loading && timestamps.length === 0) {
    return (
      <div style={{ height, display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
        <Spin />
      </div>
    )
  }

  if (timestamps.length === 0) {
    return (
      <div style={{ height, display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
        <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={emptyText} />
      </div>
    )
  }

  const labels = timestamps.map(formatClock)
  const fmt = valueFormatter ?? ((v: number) => String(v))

  const option = {
    animation: false, // 实时曲线关动画，避免刷新闪烁
    grid: { left: 56, right: 18, top: showLegend ? 34 : 16, bottom: 26 },
    legend: showLegend
      ? {
          show: true,
          top: 0,
          right: 0,
          itemWidth: 12,
          itemHeight: 8,
          textStyle: { color: CHART_THEME.text, fontSize: 11 },
        }
      : { show: false },
    tooltip: {
      trigger: 'axis',
      axisPointer: { type: 'line', lineStyle: { color: CHART_THEME.axisLine } },
      backgroundColor: CHART_THEME.tooltipBg,
      borderColor: CHART_THEME.tooltipBorder,
      textStyle: { color: CHART_THEME.textStrong, fontSize: 12 },
      // 时间 + 各序列值；值为 null 时显示"—"，明确"没数据"
      formatter: (params: Array<{ seriesName: string; value: number | null; color: string; dataIndex: number }>) => {
        const head = `<div style="margin-bottom:4px;color:${CHART_THEME.text}">${formatDateTime(
          timestamps[params[0]?.dataIndex ?? 0],
        )}</div>`
        const body = params
          .map((p) => {
            const unit = series.find((s) => s.name === p.seriesName)?.unit ?? ''
            const val = p.value === null || p.value === undefined ? '—' : `${fmt(p.value as number)}${unit}`
            return `<div style="display:flex;justify-content:space-between;gap:14px">
              <span><span style="display:inline-block;width:8px;height:8px;border-radius:50%;background:${p.color};margin-right:6px"></span>${p.seriesName}</span>
              <b style="font-family:${MONO_FONT}">${val}</b>
            </div>`
          })
          .join('')
        return head + body
      },
    },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: labels,
      axisLine: { lineStyle: { color: CHART_THEME.axisLine } },
      axisTick: { show: false },
      axisLabel: { color: CHART_THEME.text, fontSize: 11, hideOverlap: true },
    },
    yAxis: {
      type: 'value',
      max: yMax,
      min: 0,
      axisLine: { show: false },
      axisTick: { show: false },
      splitLine: { lineStyle: { color: CHART_THEME.splitLine } },
      axisLabel: {
        color: CHART_THEME.text,
        fontSize: 11,
        formatter: (v: number) => fmt(v),
      },
    },
    series: series.map((s) => ({
      name: s.name,
      type: 'line',
      data: withGaps(s.data),
      smooth: false,
      showSymbol: false, // 点太密，不画点更清爽
      connectNulls: false, // ★ 断点：拿不到的指标不连线、不补 0
      lineStyle: { width: 1.6, color: s.color },
      itemStyle: { color: s.color },
      areaStyle: s.area ? { color: s.color, opacity: 0.12 } : undefined,
      emphasis: { focus: 'series' },
    })),
  }

  return (
    <ReactECharts
      option={option}
      style={{ height, width: '100%' }}
      opts={{ renderer: 'canvas' }}
      notMerge={false} // 增量合并，避免整图重绘导致闪烁
      lazyUpdate
    />
  )
}
