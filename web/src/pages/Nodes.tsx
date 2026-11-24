import { useCallback, useEffect, useState } from 'react'
import { Card, Input, Popconfirm, Space, Table, Typography, message } from 'antd'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'
import { NODE_EVENT } from '../App'
import { HeartbeatText, NodeStatusTag } from '../components/NodeStatusTag'
import { humanDuration } from '../utils/format'
import type { MetricSample, NodeInfo, NodeStatus } from '../types'

/** NPU 型号长名 → 短名，列表里更紧凑。 */
function shortNpu(npu: string): string {
  if (npu.includes('Hexagon')) return 'Hexagon NPU'
  if (npu.includes('APU')) return 'MediaTek APU'
  if (npu.includes('NPU')) return 'NPU'
  return npu
}

/** 节点列表页：支持状态过滤、改名、删除。 */
export default function Nodes() {
  const [nodes, setNodes] = useState<NodeInfo[]>([])
  const [loading, setLoading] = useState(true)
  const [filter, setFilter] = useState<NodeStatus | 'all'>('all')
  const [editing, setEditing] = useState<string | null>(null)
  const [nameDraft, setNameDraft] = useState('')
  // 各节点最新指标（含电量 / NPU 使用率）。来自 overview 的 nodes_metrics，
  // 离线或采样陈旧的节点后端会返回 null，这里对应显示 "—"。
  const [metrics, setMetrics] = useState<Record<string, MetricSample | null>>({})
  const navigate = useNavigate()

  const load = useCallback(async (showSpin = false) => {
    if (showSpin) setLoading(true)
    try {
      const data = await api.listNodes()
      setNodes(data.items)
    } catch {
      /* 连接失败时保留上次数据，避免页面闪空 */
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load(true)
    const t = window.setInterval(() => load(false), 5000)
    return () => window.clearInterval(t)
  }, [load])

  useEffect(() => {
    const handler = () => load(false)
    window.addEventListener(NODE_EVENT, handler)
    return () => window.removeEventListener(NODE_EVENT, handler)
  }, [load])

  // 拉取各节点最新指标，供「电量 / NPU」列展示。
  const loadMetrics = useCallback(async () => {
    try {
      const d = await api.overview()
      setMetrics(d.nodes_metrics ?? {})
    } catch {
      /* 拉取失败保留上次数据，避免页面闪空 */
    }
  }, [])

  useEffect(() => {
    loadMetrics()
    const t = window.setInterval(loadMetrics, 5000)
    return () => window.clearInterval(t)
  }, [loadMetrics])

  const saveName = async (nodeId: string) => {
    const name = nameDraft.trim()
    if (!name) {
      setEditing(null)
      return
    }
    try {
      await api.renameNode(nodeId, name)
      message.success('已更新节点名称')
      setEditing(null)
      load(false)
    } catch {
      message.error('更新失败，请检查平台连接')
    }
  }

  const remove = async (nodeId: string) => {
    try {
      await api.deleteNode(nodeId)
      message.success('已删除节点记录')
      load(false)
    } catch {
      message.error('删除失败')
    }
  }

  const shown = filter === 'all' ? nodes : nodes.filter((n) => n.status === filter)

  const columns = [
    {
      title: '名称',
      dataIndex: 'name',
      key: 'name',
      render: (_: string, row: NodeInfo) => (
        <Space direction="vertical" size={0}>
          {editing === row.node_id ? (
            <Input
              size="small"
              autoFocus
              value={nameDraft}
              onChange={(e) => setNameDraft(e.target.value)}
              onPressEnter={() => saveName(row.node_id)}
              onBlur={() => saveName(row.node_id)}
              style={{ width: 160 }}
            />
          ) : (
            <Space size={6}>
              <a onClick={() => navigate(`/nodes/${row.node_id}`)}>{row.name || row.node_id}</a>
              {row.is_simulated && (
                <Typography.Text type="secondary" style={{ fontSize: 11 }}>
                  [模拟]
                </Typography.Text>
              )}
            </Space>
          )}
          <Typography.Text type="secondary" className="mono" style={{ fontSize: 11 }}>
            {row.node_id}
          </Typography.Text>
        </Space>
      ),
    },
    {
      title: '状态',
      dataIndex: 'status',
      key: 'status',
      width: 110,
      render: (s: NodeStatus) => <NodeStatusTag status={s} />,
    },
    {
      title: '电量',
      key: 'battery',
      width: 96,
      render: (_: unknown, row: NodeInfo) => {
        const m = metrics[row.node_id]
        // overview 接口的电量/状态在 extra 里，不在顶层（public_json 只留了 12 个核心字段）
        const extra = m?.extra
        const pct =
          extra && typeof extra.battery_percent === 'number'
            ? (extra.battery_percent as number)
            : undefined
        if (pct == null) {
          return <span style={{ color: '#8c8c8c' }}>—</span>
        }
        const color = pct <= 20 ? '#ff4d4f' : pct <= 50 ? '#faad14' : '#2ea043'
        const status = String(extra?.battery_status ?? '').toLowerCase()
        const charging = status === 'charging' || status === 'full'
        return (
          <Space size={4}>
            <span className="mono" style={{ color, fontSize: 13 }}>
              {pct.toFixed(0)}%
            </span>
            {charging && (
              <span style={{ fontSize: 12, color: '#2ea043' }} title="充电中">
                ⚡
              </span>
            )}
          </Space>
        )
      },
    },
    {
      title: 'NPU',
      key: 'npu',
      width: 120,
      render: (_: unknown, row: NodeInfo) => {
        const m = metrics[row.node_id]
        const usage = m?.npu_usage
        const name = row.npu && row.npu !== 'Unknown' ? shortNpu(row.npu) : ''
        return (
          <Space direction="vertical" size={0}>
            <span
              className="mono"
              style={{ fontSize: 13, color: usage == null ? '#8c8c8c' : '#4f9cf9' }}
            >
              {usage == null ? '—' : `${usage.toFixed(0)}%`}
            </span>
            {name && <span style={{ fontSize: 11, color: '#8c8c8c' }}>{name}</span>}
          </Space>
        )
      },
    },
    {
      title: '最后心跳',
      dataIndex: 'last_heartbeat',
      key: 'hb',
      width: 220,
      render: (_: string, row: NodeInfo) => (
        <HeartbeatText lastHeartbeat={row.last_heartbeat} status={row.status} />
      ),
    },
    {
      title: '在线时长',
      dataIndex: 'uptime_seconds',
      key: 'up',
      width: 120,
      render: (v: number, row: NodeInfo) =>
        row.status === 'online' ? humanDuration(v) : <span style={{ color: '#8c8c8c' }}>—</span>,
    },
    { title: '型号', dataIndex: 'model', key: 'model', render: (v: string) => v || '—' },
    { title: 'SoC', dataIndex: 'soc', key: 'soc', render: (v: string) => v || '—' },
    {
      title: 'QNN / Root',
      key: 'qnn',
      width: 140,
      render: (_: unknown, row: NodeInfo) => (
        <Space direction="vertical" size={0} style={{ fontSize: 12 }}>
          <span className="mono">{row.qnn_version || '未识别'}</span>
          <span style={{ color: '#8c8c8c' }}>
            {/* -1 是"未判定"，不是"没有" —— 两者混在一起会让人误判设备能力 */}
            {row.is_root === 1 ? '有 root' : row.is_root === 0 ? '无 root' : 'root 未判定'}
          </span>
        </Space>
      ),
    },
    {
      title: 'Android',
      dataIndex: 'android_version',
      key: 'android',
      width: 90,
      render: (v: string) => v || '—',
    },
    {
      title: 'IP',
      dataIndex: 'ip',
      key: 'ip',
      width: 150,
      render: (v: string) => <span className="mono">{v || '—'}</span>,
    },
    {
      title: '操作',
      key: 'action',
      width: 130,
      render: (_: unknown, row: NodeInfo) => (
        <Space size={8}>
          <a
            onClick={() => {
              setEditing(row.node_id)
              setNameDraft(row.name)
            }}
          >
            改名
          </a>
          <Popconfirm
            title="删除该节点记录？"
            description="仅删除平台侧记录，节点重新上报后会再次出现。"
            onConfirm={() => remove(row.node_id)}
            okText="删除"
            cancelText="取消"
          >
            <a style={{ color: '#ff7875' }}>删除</a>
          </Popconfirm>
        </Space>
      ),
    },
  ]

  return (
    <Card
      title="节点管理"
      size="small"
      style={{ background: '#1a212b', borderColor: '#2a3444' }}
      extra={
        <Space size={8}>
          <span style={{ fontSize: 12, color: '#8c8c8c' }}>状态过滤</span>
          <select
            value={filter}
            onChange={(e: React.ChangeEvent<HTMLSelectElement>) =>
              setFilter(e.target.value as NodeStatus | 'all')
            }
            style={{
              background: '#14181f',
              color: '#e6edf3',
              border: '1px solid #2a3444',
              borderRadius: 6,
              padding: '3px 8px',
            }}
          >
            <option value="all">全部（{nodes.length}）</option>
            <option value="online">在线（{nodes.filter((n) => n.status === 'online').length}）</option>
            <option value="offline">
              离线（{nodes.filter((n) => n.status === 'offline').length}）
            </option>
            <option value="never_seen">
              从未上线（{nodes.filter((n) => n.status === 'never_seen').length}）
            </option>
          </select>
        </Space>
      }
    >
      <Table
        rowKey="node_id"
        size="small"
        loading={loading && nodes.length === 0}
        columns={columns as never}
        dataSource={shown}
        pagination={false}
      />
    </Card>
  )
}
