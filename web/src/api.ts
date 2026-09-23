/**
 * API 封装。
 *
 * token 来源优先级：localStorage > URL 查询参数 ?token= > 默认开发 token。
 * 用 URL 传一次 token 后会写入 localStorage 并从地址栏抹掉，避免反复粘贴。
 */

import axios from 'axios'
import type {
  ClusterSeriesOut,
  MetricSeriesOut,
  NodeInfo,
  NodeListOut,
  OverviewOut,
  SystemInfoOut,
} from './types'

const DEFAULT_TOKEN = 'edge-cluster-dev-token'
const TOKEN_KEY = 'edge_cluster_token'

function resolveToken(): string {
  try {
    // 1) URL 参数（首次访问带入，随后落盘并从地址栏清理）
    const url = new URL(window.location.href)
    const fromUrl = url.searchParams.get('token')
    if (fromUrl) {
      localStorage.setItem(TOKEN_KEY, fromUrl)
      url.searchParams.delete('token')
      window.history.replaceState({}, '', url.toString())
      return fromUrl
    }
    // 2) localStorage
    const saved = localStorage.getItem(TOKEN_KEY)
    if (saved) return saved
  } catch {
    /* 忽略：取不到就用默认值 */
  }
  return DEFAULT_TOKEN
}

export const TOKEN = resolveToken()

export const http = axios.create({
  baseURL: '/api/v1',
  timeout: 15000,
  headers: { 'X-Auth-Token': TOKEN },
})

http.interceptors.response.use(
  (resp) => resp,
  (error) => {
    const status = error?.response?.status
    if (status === 401) {
      // eslint-disable-next-line no-console
      console.error('[api] 鉴权失败：请检查 token 是否与平台一致')
    }
    return Promise.reject(error)
  },
)

export const api = {
  // ---- 系统 ----
  systemInfo: () => http.get<SystemInfoOut>('/system/info').then((r) => r.data),
  /** 首页大屏聚合数据（节点统计 + 资源汇总 + 告警 + 异常节点） */
  overview: () => http.get<OverviewOut>('/system/overview').then((r) => r.data),
  /** 集群资源时间序列（大屏趋势图） */
  clusterMetrics: (minutes = 30) =>
    http.get<ClusterSeriesOut>('/system/cluster-metrics', { params: { minutes } }).then((r) => r.data),
  serverLogs: (lines = 200) =>
    http.get<{ lines: string[]; total_lines: number; log_file: string }>('/system/server-logs', {
      params: { lines },
    }).then((r) => r.data),

  // ---- 节点 ----
  listNodes: (status?: string) =>
    http.get<NodeListOut>('/nodes', { params: status ? { status } : {} }).then((r) => r.data),
  getNode: (nodeId: string) =>
    http.get<NodeInfo>(`/nodes/${nodeId}`).then((r) => r.data),
  renameNode: (nodeId: string, name: string) =>
    http.patch<NodeInfo>(`/nodes/${nodeId}`, { name }).then((r) => r.data),
  deleteNode: (nodeId: string) =>
    http.delete(`/nodes/${nodeId}`).then((r) => r.data),
  nodeMetrics: (nodeId: string, minutes = 30) =>
    http
      .get<MetricSeriesOut>(`/nodes/${nodeId}/metrics`, { params: { minutes, limit: 1200 } })
      .then((r) => r.data),
  nodeServices: (nodeId: string) =>
    http.get(`/nodes/${nodeId}/services`).then((r) => r.data),
}

/** Dashboard 实时通道地址 */
export function dashboardWsUrl(): string {
  const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${proto}//${window.location.host}/ws/dashboard?token=${encodeURIComponent(TOKEN)}`
}
