import { Tag, Tooltip } from 'antd'
import type { NodeStatus, ServiceStatus } from '../types'
import { NODE_STATUS_META, SERVICE_STATUS_META } from '../types'

/**
 * 节点状态标签。
 *
 * 关键要求（任务书 §4）：必须明确区分「从未上线 / 在线 / 离线」三态，
 * 离线节点要显眼标注，绝不能用旧数据冒充在线。
 */
export function NodeStatusTag({
  status,
  showDot = true,
}: {
  status: NodeStatus
  showDot?: boolean
}) {
  const meta = NODE_STATUS_META[status] ?? { text: status, color: 'default' }
  return (
    <Tag color={meta.color} style={{ marginInlineEnd: 0 }}>
      {showDot && <span className={`status-dot ${status}`} style={{ marginRight: 6 }} />}
      {meta.text}
    </Tag>
  )
}

export function ServiceStatusTag({ status }: { status: ServiceStatus }) {
  const meta = SERVICE_STATUS_META[status] ?? { text: status, color: 'default' }
  return (
    <Tag color={meta.color} style={{ marginInlineEnd: 0 }}>
      {meta.text}
    </Tag>
  )
}

/** 心跳显示：在线显示"几秒前"，离线显式标注离线时刻 */
export function HeartbeatText({
  lastHeartbeat,
  status,
}: {
  lastHeartbeat: string | null
  status: NodeStatus
}) {
  if (!lastHeartbeat) {
    return (
      <Tooltip title="该节点从未上报过心跳">
        <span style={{ color: '#8c8c8c' }}>无记录</span>
      </Tooltip>
    )
  }
  const t = new Date(lastHeartbeat.endsWith('Z') ? lastHeartbeat : `${lastHeartbeat}Z`)
  const diff = Math.max(0, Math.floor((Date.now() - t.getTime()) / 1000))
  const display = t.toLocaleString('zh-CN', { hour12: false })
  const ago = diff < 60 ? `${diff} 秒前` : diff < 3600 ? `${Math.floor(diff / 60)} 分钟前` : `${Math.floor(diff / 3600)} 小时前`
  return (
    <Tooltip title={`最后心跳：${display}`}>
      <span style={{ color: status === 'online' ? '#d9d9d9' : '#8c8c8c' }}>
        {status === 'online' ? ago : `${display}（已离线）`}
      </span>
    </Tooltip>
  )
}
