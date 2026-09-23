/** 单位格式化工具，全站共用。 */

export function humanBytes(n: number | null | undefined, digits = 1): string {
  if (n === null || n === undefined) return '—'
  const v = Number(n)
  if (!isFinite(v)) return '—'
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']
  let x = Math.abs(v)
  let i = 0
  while (x >= 1024 && i < units.length - 1) {
    x /= 1024
    i += 1
  }
  return `${x.toFixed(i === 0 ? 0 : digits)} ${units[i]}`
}

export function humanPercent(n: number | null | undefined): string {
  if (n === null || n === undefined) return '—'
  return `${Number(n).toFixed(1)}%`
}

export function humanTemp(n: number | null | undefined): string {
  if (n === null || n === undefined) return '—'
  return `${Number(n).toFixed(1)} ℃`
}

export function humanDuration(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || seconds <= 0) return '—'
  const d = Math.floor(seconds / 86400)
  const h = Math.floor((seconds % 86400) / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const s = Math.floor(seconds % 60)
  if (d > 0) return `${d} 天 ${h} 小时`
  if (h > 0) return `${h} 小时 ${m} 分`
  if (m > 0) return `${m} 分 ${s} 秒`
  return `${s} 秒`
}

/** 计算使用率百分比，拿不到返回 null（不造假） */
export function ratio(used: number | null | undefined, total: number | null | undefined): number | null {
  if (!used || !total || total <= 0) return null
  return Math.min(100, Math.max(0, (used / total) * 100))
}

/**
 * 解析后端时间戳。
 *
 * 后端统一存/返回 **naive UTC** ISO 串（如 `2026-09-16T02:39:27.549611`），
 * 不带时区后缀。直接 `new Date(s)` 会被浏览器按本地时区解释而整体偏移，
 * 因此这里显式补 `Z` 标记为 UTC。
 */
export function parseUtc(iso: string | null | undefined): Date | null {
  if (!iso) return null
  const s = iso.endsWith('Z') || /[+-]\d{2}:\d{2}$/.test(iso) ? iso : `${iso}Z`
  const d = new Date(s)
  return isNaN(d.getTime()) ? null : d
}

/** 时间戳 → 本地 `HH:mm:ss`（图表 X 轴用） */
export function formatClock(iso: string | null | undefined): string {
  const d = parseUtc(iso)
  if (!d) return ''
  return d.toLocaleTimeString('zh-CN', { hour12: false })
}

/** 时间戳 → 本地 `MM-DD HH:mm:ss`（Tooltip 用） */
export function formatDateTime(iso: string | null | undefined): string {
  const d = parseUtc(iso)
  if (!d) return ''
  return d.toLocaleString('zh-CN', { hour12: false })
}
