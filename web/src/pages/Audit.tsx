import { useCallback, useEffect, useState } from 'react'
import {
  Alert,
  Button,
  Card,
  Input,
  Select,
  Space,
  Table,
  Tag,
  Tooltip,
  Typography,
  message,
} from 'antd'
import { ReloadOutlined } from '@ant-design/icons'
import { api, errText } from '../api'
import { formatDateTime } from '../utils/format'
import type { AuditEntry } from '../types'

const CARD_STYLE = { background: '#1a212b', borderColor: '#2a3444' } as const

/** 动作前缀 → 人话。**用前缀匹配**，后端加子动作时不用改这里 */
const ACTION_GROUPS: { value: string; label: string }[] = [
  { value: '', label: '全部' },
  { value: 'deployment.', label: '部署' },
  { value: 'service.', label: '服务' },
  { value: 'model.', label: '模型' },
  { value: 'file.', label: '文件' },
  { value: 'node.', label: '节点' },
]

/**
 * 操作审计页（需求 §8）。
 *
 * 记录从第一阶段起就在写，但一直没有读的入口 —— 一份查不了的审计，
 * 在需要追责的时候等于不存在。这个页面就是那个入口。
 */
export default function Audit() {
  const [items, setItems] = useState<AuditEntry[]>([])
  const [total, setTotal] = useState(0)
  const [loading, setLoading] = useState(true)
  const [action, setAction] = useState('')
  const [ok, setOk] = useState<number | undefined>(undefined)
  const [nodeId, setNodeId] = useState('')
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(50)

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const d = await api.listAudit({
        limit: pageSize,
        offset: (page - 1) * pageSize,
        action: action || undefined,
        node_id: nodeId || undefined,
        ok,
      })
      setItems(d.items ?? [])
      setTotal(d.total ?? 0)
    } catch (e) {
      message.error(errText(e, '加载审计记录失败'))
    } finally {
      setLoading(false)
    }
  }, [action, nodeId, ok, page, pageSize])

  useEffect(() => {
    load()
  }, [load])

  const columns = [
    {
      title: '时间',
      dataIndex: 'ts',
      key: 'ts',
      width: 175,
      render: (v: string) => (v ? formatDateTime(v) : '—'),
    },
    {
      title: '结果',
      dataIndex: 'ok',
      key: 'ok',
      width: 80,
      render: (v: number) =>
        v === 1 ? <Tag color="success">成功</Tag> : <Tag color="error">失败</Tag>,
    },
    {
      title: '动作',
      dataIndex: 'action',
      key: 'action',
      width: 190,
      render: (v: string) => (
        <Typography.Text className="mono" style={{ fontSize: 12 }}>
          {v || '—'}
        </Typography.Text>
      ),
    },
    {
      title: '对象',
      key: 'target',
      width: 260,
      render: (_: unknown, r: AuditEntry) => (
        <Space direction="vertical" size={0}>
          <span style={{ fontSize: 12 }}>
            {r.target_type || '—'}
            {r.target_id ? ' / ' : ''}
            <span className="mono" style={{ fontSize: 11 }}>
              {r.target_id}
            </span>
          </span>
          {r.node_id && (
            <Typography.Text type="secondary" className="mono" style={{ fontSize: 11 }}>
              node: {r.node_id}
            </Typography.Text>
          )}
        </Space>
      ),
    },
    {
      title: '操作者 / 来源',
      key: 'actor',
      width: 180,
      render: (_: unknown, r: AuditEntry) => (
        <Space direction="vertical" size={0}>
          <span style={{ fontSize: 12 }}>{r.actor || '—'}</span>
          <Typography.Text type="secondary" className="mono" style={{ fontSize: 11 }}>
            {r.client_ip || '—'}
          </Typography.Text>
        </Space>
      ),
    },
    {
      title: '说明 / 明细',
      key: 'detail',
      render: (_: unknown, r: AuditEntry) => (
        <Space direction="vertical" size={0} style={{ width: '100%' }}>
          {r.message && <span style={{ fontSize: 12 }}>{r.message}</span>}
          {r.detail && Object.keys(r.detail).length > 0 && (
            <Tooltip title={<pre style={{ margin: 0, fontSize: 11 }}>{JSON.stringify(r.detail, null, 2)}</pre>}>
              <Typography.Text type="secondary" className="mono" style={{ fontSize: 11 }}>
                {summarize(r.detail)}
              </Typography.Text>
            </Tooltip>
          )}
          {!r.message && !r.detail && '—'}
        </Space>
      ),
    },
  ]

  return (
    <Card
      title="操作审计"
      size="small"
      style={CARD_STYLE}
      extra={
        <Button icon={<ReloadOutlined />} onClick={load}>
          刷新
        </Button>
      }
    >
      <Alert
        type="info"
        showIcon
        style={{ marginBottom: 12 }}
        message="只记录会改变状态的操作（部署、启停、删除、改配置、文件读写、远程命令），读操作不记。"
        description="失败的写操作同样留痕 —— 「谁试过但没成功」也是审计信息，甚至更重要。审计只能由系统写入，没有任何接口能修改或补录。"
      />
      <Space style={{ marginBottom: 12 }} wrap>
        <Select style={{ width: 140 }} value={action} onChange={(v) => { setAction(v); setPage(1) }} options={ACTION_GROUPS} />
        <Select
          style={{ width: 130 }}
          value={ok === undefined ? 'all' : String(ok)}
          onChange={(v) => {
            setOk(v === 'all' ? undefined : Number(v))
            setPage(1)
          }}
          options={[
            { value: 'all', label: '全部结果' },
            { value: '1', label: '只看成功' },
            { value: '0', label: '只看失败' },
          ]}
        />
        <Input
          style={{ width: 200 }}
          placeholder="按节点 ID 过滤"
          value={nodeId}
          onChange={(e) => setNodeId(e.target.value)}
          onPressEnter={() => setPage(1)}
          allowClear
        />
        <Button onClick={() => setPage(1)}>应用过滤</Button>
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          共 {total} 条
        </Typography.Text>
      </Space>

      <Table
        rowKey="id"
        size="small"
        loading={loading}
        columns={columns as never}
        dataSource={items}
        pagination={{
          current: page,
          pageSize,
          total,
          showSizeChanger: true,
          pageSizeOptions: ['20', '50', '100', '200'],
          onChange: (p, ps) => {
            setPage(p)
            setPageSize(ps)
          },
        }}
      />
    </Card>
  )
}

/** 把明细压成一行短摘要，完整内容放进 Tooltip —— 表格里塞满 JSON 没法看 */
function summarize(detail: Record<string, unknown>): string {
  const parts: string[] = []
  for (const [k, v] of Object.entries(detail)) {
    if (v === null || v === undefined || v === '') continue
    const s = Array.isArray(v) ? v.join(',') : typeof v === 'object' ? JSON.stringify(v) : String(v)
    parts.push(`${k}=${s.length > 40 ? `${s.slice(0, 40)}…` : s}`)
    if (parts.length >= 3) break
  }
  return parts.join('  ') || '—'
}
