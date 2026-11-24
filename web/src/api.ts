/**
 * API 封装。
 *
 * token 来源优先级：localStorage > URL 查询参数 ?token= > 默认开发 token。
 * 用 URL 传一次 token 后会写入 localStorage 并从地址栏抹掉，避免反复粘贴。
 *
 * 这里的方法与后端路由**一一对应**。加接口时两边一起改，
 * 名字不一致会导致"界面点不动而控制台一片红"——TS 只能查到类型，查不到 404。
 */

import axios from 'axios'
import type {
  AuditListOut,
  BackupInspectOut,
  BackupRestoreOut,
  BackupStatusOut,
  ClusterSeriesOut,
  CompatResult,
  DeploymentListOut,
  DeploymentOut,
  DeployQueueStats,
  FsDuOut,
  FsListOut,
  FsRootsOut,
  FsStatOut,
  MetricSeriesOut,
  ModelListOut,
  ModelOut,
  ModelPatchIn,
  NodeInfo,
  NodeListOut,
  OverviewOut,
  PrecheckOut,
  ServiceCreateIn,
  ServiceLogsOut,
  ServiceOut,
  ServiceStatsOut,
  SystemInfoOut,
  TransferListOut,
  TransferOut,
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

/**
 * 从 axios 错误里取后端给的**人话**。
 *
 * 后端几乎所有业务错误的 `detail` 都是写给用户看的（"服务仍在 running 状态，
 * 请先停止再删除"）。直接把它显示出来，比前端自己编一句"操作失败"有用得多 ——
 * 用户需要知道**为什么**不行。
 */
export function errText(e: unknown, fallback = '操作失败'): string {
  const anyE = e as { response?: { data?: { detail?: unknown }; status?: number } }
  const detail = anyE?.response?.data?.detail
  if (typeof detail === 'string' && detail) return detail
  if (Array.isArray(detail)) {
    // FastAPI 校验错误：detail 是 [{loc, msg, type}]，挑出 msg 拼起来
    const msgs = detail
      .map((d) => (d && typeof d === 'object' && 'msg' in d ? String((d as { msg: unknown }).msg) : ''))
      .filter(Boolean)
    if (msgs.length) return msgs.join('；')
  }
  const status = anyE?.response?.status
  if (status === 401) return '鉴权失败：token 与平台不一致'
  if (status === 404) return '对象不存在（可能已被删除）'
  if (status === 409) return '当前状态不允许这个操作'
  if (status === 413) return '文件超过平台允许的单次传输上限'
  if (status === 503) return '节点离线或未连接'
  if (status === 504) return '设备未在规定时间内回执（可能卡住了）'
  const msg = (e as { message?: string })?.message
  return msg ? `${fallback}：${msg}` : fallback
}

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

  // ---- 备份与恢复（A4）----
  backupStatus: () => http.get<BackupStatusOut>('/system/backup/status').then((r) => r.data),
  /** 触发一次备份并从浏览器下载 zip。用 blob 而不是直接打开链接：
   *  链接方式拿不到 X-Auth-Token 头，会被 401 挡掉。 */
  downloadBackup: async (includeModels = false) => {
    const r = await http.get('/system/backup', {
      params: { include_models: includeModels },
      responseType: 'blob',
      timeout: 0,
      // 从 Content-Disposition 里取后端定的文件名（带时间戳），取不到再兜底
      transformResponse: (d) => d,
    })
    const cd = String(r.headers['content-disposition'] || '')
    const m = /filename\*?=(?:UTF-8'')?"?([^";]+)"?/i.exec(cd)
    return { blob: r.data as Blob, name: m?.[1] || `edge-cluster-backup.zip` }
  },
  /** 只校验备份包，不改动任何数据 */
  inspectBackup: (file: File) => {
    const fd = new FormData()
    fd.append('file', file)
    return http
      .post<BackupInspectOut>('/system/backup/inspect', fd, { timeout: 0 })
      .then((r) => r.data)
  },
  /** 从备份包恢复。confirm=false 时后端只做校验、一个字节都不动 */
  restoreBackup: (file: File, confirm: boolean) => {
    const fd = new FormData()
    fd.append('file', file)
    return http
      .post<BackupRestoreOut>('/system/backup/restore', fd, {
        params: { confirm },
        timeout: 0,
      })
      .then((r) => r.data)
  },

  // ---- 审计（需求 §8）----
  listAudit: (params: {
    limit?: number
    offset?: number
    action?: string
    node_id?: string
    actor?: string
    ok?: number
  } = {}) => http.get<AuditListOut>('/audit', { params }).then((r) => r.data),
  auditActions: () => http.get<{ items: string[] }>('/audit/actions').then((r) => r.data),

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
    http.get<{ items: ServiceOut[]; total: number }>(`/nodes/${nodeId}/services`).then((r) => r.data),

  // ---- 模型库（Model Registry，需求 §5）----
  listModels: () =>
    http.get<ModelListOut>('/models').then((r) => r.data),
  getModel: (modelId: string) =>
    http.get<ModelOut>(`/models/${modelId}`).then((r) => r.data),
  deleteModel: (modelId: string) =>
    http.delete(`/models/${modelId}`).then((r) => r.data),
  uploadModel: (form: FormData, onProgress?: (pct: number) => void) =>
    http
      .post<ModelOut>('/models', form, {
        headers: { 'Content-Type': 'multipart/form-data' },
        // 模型动辄 GB 级，绝不能套用 axios 实例的 15s 默认超时：那会把大文件
        // 上传在 15 秒处直接掐断（现象就是「上传很慢 / 一直失败」）。
        timeout: 0,
        onUploadProgress: (e) => {
          if (onProgress && e.total) {
            onProgress(Math.min(100, Math.round((e.loaded / e.total) * 100)))
          }
        },
      })
      .then((r) => r.data),
  /** 增量更新登记信息：只提交要改的字段（后端 exclude_unset，未给的不会被清空） */
  patchModel: (modelId: string, patch: ModelPatchIn) =>
    http.patch<ModelOut>(`/models/${modelId}`, patch).then((r) => r.data),
  /** 单模型兼容性预检（不落库、不下发） */
  modelCompat: (modelId: string, nodeIds: string[]) =>
    http
      .post<PrecheckOut>(`/models/${modelId}/compat`, { node_ids: nodeIds })
      .then((r) => r.data),
  /** 上传 Deployment Package 附属物（可一次多个） */
  uploadManifest: (modelId: string, kind: string, files: File[]) => {
    const form = new FormData()
    form.append('kind', kind)
    files.forEach((f) => form.append('files', f))
    return http
      .post<ModelOut>(`/models/${modelId}/manifest`, form, {
        headers: { 'Content-Type': 'multipart/form-data' },
        // 附属物可能包含 DLC/lib 等大文件，同样不能套 15s 默认超时。
        timeout: 0,
      })
      .then((r) => r.data)
  },
  deleteManifest: (modelId: string, name: string) =>
    http
      .delete<ModelOut>(`/models/${modelId}/manifest/${encodeURIComponent(name)}`)
      .then((r) => r.data),

  // ---- 部署任务（需求 §6）----
  listDeployments: () =>
    http.get<DeploymentListOut>('/deployments').then((r) => r.data),
  getDeployment: (taskId: string) =>
    http.get<DeploymentOut>(`/deployments/${taskId}`).then((r) => r.data),
  createDeployment: (body: { model_id: string; node_ids: string[]; note?: string }) =>
    http.post<DeploymentOut>('/deployments', body).then((r) => r.data),
  retryDeployment: (taskId: string) =>
    http.post<DeploymentOut>(`/deployments/${taskId}/retry`).then((r) => r.data),
  /** 取消：只把**还没到终态**的节点标为 cancelled，已就绪的不动 */
  cancelDeployment: (taskId: string) =>
    http
      .post<DeploymentOut & { notified: Record<string, boolean> }>(`/deployments/${taskId}/cancel`)
      .then((r) => r.data),
  /** 删除已结束的部署任务（failed/cancelled/succeeded/partial），清掉列表里堆积的旧记录 */
  deleteDeployment: (taskId: string) =>
    http.post<{ deleted: boolean; task_id: string }>(`/deployments/${taskId}/delete`).then((r) => r.data),
  /** 下发前预检（只读）：让用户在选节点阶段就看到哪台不兼容 */
  precheckDeployment: (body: { model_id: string; node_ids: string[]; note?: string }) =>
    http.post<PrecheckOut>('/deployments/precheck', body).then((r) => r.data),
  deployQueueStats: () =>
    http.get<DeployQueueStats>('/deployments/queue/stats').then((r) => r.data),

  // ---- 服务（需求 §4 / §7）----
  listServices: (nodeId?: string) =>
    http
      .get<ServiceOut[]>('/services', { params: nodeId ? { node_id: nodeId } : {} })
      .then((r) => r.data),
  getService: (serviceId: string) =>
    http.get<ServiceOut>(`/services/${serviceId}`).then((r) => r.data),
  createService: (body: ServiceCreateIn) =>
    http.post<ServiceOut>('/services', body).then((r) => r.data),
  /** 删除服务定义。运行中的服务后端会回 409 —— 必须先停，避免留下看不见的孤儿进程 */
  deleteService: (serviceId: string) =>
    http.delete<{ deleted: string; agent_notified: boolean }>(`/services/${serviceId}`).then((r) => r.data),
  serviceAction: (serviceId: string, action: 'start' | 'stop' | 'restart') =>
    http
      .post<ServiceOut & { command_id: string; delivered: boolean }>(
        `/services/${serviceId}/${action}`,
      )
      .then((r) => r.data),
  serviceLogs: (serviceId: string, lines = 200) =>
    http
      .get<ServiceLogsOut>(`/services/${serviceId}/logs`, { params: { lines } })
      .then((r) => r.data),
  serviceCheck: (serviceId: string) =>
    http
      .post<ServiceOut & { probe: { url: string; ok: boolean; detail: string } }>(
        `/services/${serviceId}/check`,
      )
      .then((r) => r.data),
  gatewayStats: (serviceId: string) =>
    http.get<ServiceStatsOut>(`/gateway/${serviceId}/stats`).then((r) => r.data),

  // ---- 文件管理（需求 §4）----
  fsRoots: (nodeId: string) =>
    http.get<FsRootsOut>(`/nodes/${nodeId}/files/roots`).then((r) => r.data),
  /* `path` 用 index 参数而不是 null 值：后端把它当 query string，
     传 null 会变成字面量 "null" 被当成路径。 */
  fsList: (nodeId: string, path: string, offset = 0, limit = 500) =>
    http
      .get<FsListOut>(`/nodes/${nodeId}/files`, { params: { path, offset, limit } })
      .then((r) => r.data),
  fsStat: (nodeId: string, path: string) =>
    http.get<FsStatOut>(`/nodes/${nodeId}/files/stat`, { params: { path } }).then((r) => r.data),
  fsDu: (nodeId: string, path: string) =>
    http.get<FsDuOut>(`/nodes/${nodeId}/files/du`, { params: { path }, timeout: 60000 }).then((r) => r.data),
  fsMkdir: (nodeId: string, path: string, parents = true) =>
    http.post(`/nodes/${nodeId}/files/mkdir`, { path, parents }).then((r) => r.data),
  /** 删除。目录必须显式 recursive=true —— 后端默认拒绝，防误删 */
  fsDelete: (nodeId: string, path: string, recursive = false) =>
    http.post(`/nodes/${nodeId}/files/delete`, { path, recursive }).then((r) => r.data),
  fsCopy: (nodeId: string, src: string, dst: string, overwrite = false) =>
    http.post(`/nodes/${nodeId}/files/copy`, { src, dst, overwrite }).then((r) => r.data),
  fsMove: (nodeId: string, src: string, dst: string, overwrite = false) =>
    http.post(`/nodes/${nodeId}/files/move`, { src, dst, overwrite }).then((r) => r.data),
  /** 从设备取文件到平台：建会话 + 让 Agent 主动上传，随后轮询 transfers 看结果 */
  fsPull: (nodeId: string, path: string) =>
    http.post<TransferOut>(`/nodes/${nodeId}/files/pull`, { path }).then((r) => r.data),
  /** 从平台推文件到设备：multipart 直传（平台先暂存，再让 Agent 拉） */
  fsPush: (nodeId: string, path: string, file: File, overwrite = false) => {
    const form = new FormData()
    form.append('path', path)
    form.append('overwrite', String(overwrite))
    form.append('file', file)
    return http
      .post<TransferOut>(`/nodes/${nodeId}/files/push`, form, {
        headers: { 'Content-Type': 'multipart/form-data' },
        timeout: 120000,
      })
      .then((r) => r.data)
  },
  listTransfers: (nodeId?: string, limit = 50) =>
    http
      .get<TransferListOut>('/files/transfers', { params: { node_id: nodeId ?? '', limit } })
      .then((r) => r.data),
  getTransfer: (transferId: string) =>
    http.get<TransferOut>(`/files/transfers/${transferId}`).then((r) => r.data),
  cancelTransfer: (transferId: string) =>
    http.delete<{ transfer_id: string; status: string }>(`/files/transfers/${transferId}`).then((r) => r.data),
}

/**
 * 平台取回文件的下载地址（浏览器直接打开）。
 *
 * 走 `http` 拿不到文件——axios 会把响应体当 JSON 解析。
 * 这里给出带 token 的直链让浏览器自己下载，服务端已经按 token 鉴权。
 */
export function transferDownloadUrl(transferId: string, filename?: string): string {
  const params = new URLSearchParams({ token: TOKEN })
  if (filename) params.set('filename', filename)
  return `/api/v1/files/transfers/${transferId}/download?${params.toString()}`
}

/** 模型文件直链（管理端下载，走共享 token） */
export function modelDownloadUrl(modelId: string): string {
  return `/api/v1/models/${modelId}/download?token=${encodeURIComponent(TOKEN)}`
}

/** Deployment Package 附属物直链 */
export function manifestDownloadUrl(modelId: string, name: string): string {
  return `/api/v1/models/${modelId}/manifest/${encodeURIComponent(name)}?token=${encodeURIComponent(TOKEN)}`
}

/** Dashboard 实时通道地址 */
export function dashboardWsUrl(): string {
  const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${proto}//${window.location.host}/ws/dashboard?token=${encodeURIComponent(TOKEN)}`
}
