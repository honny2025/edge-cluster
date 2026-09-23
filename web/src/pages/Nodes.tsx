import { useCallback, useEffect, useState } from 'react'
import { Card, Input, Popconfirm, Space, Table, Typography, message } from 'antd'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'
import { NODE_EVENT } from '../App'
import { HeartbeatText, NodeStatusTag } from '../components/NodeStatusTag'
import { humanDuration } from '../utils/format'
import type { NodeInfo, NodeStatus } from '../types'

/** 节点列表页：支持状态过滤、改名、删除。 */
export default function Nodes() {
  const [nodes, setNodes] = useState<NodeInfo[]>([])
  const [loading, setLoading] = useState(true)
  const [filter, setFilter] = useState<NodeStatus | 'all'>('all')
  const [editing, setEditing] = useState<string | null>(null)
  const [nameDraft, setNameDraft] = useState('')
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
