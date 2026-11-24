import { useCallback, useEffect, useState } from 'react'
import {
  Alert,
  Button,
  Card,
  Input,
  Modal,
  Popconfirm,
  Progress,
  Select,
  Space,
  Table,
  Tag,
  Typography,
  message,
} from 'antd'
import { api, errText } from '../api'
import { formatDateTime, humanBytes } from '../utils/format'
import type {
  CompatCheck,
  CompatVerdict,
  DeployNodeOut,
  DeploymentOut,
  ModelOut,
  NodeInfo,
  PrecheckNodeResult,
} from '../types'
import { DEPLOY_NODE_STATUS_META, DEPLOY_TASK_STATUS_META, DEPLOY_TERMINAL_NODE_STATES } from '../types'

const CARD_STYLE = { background: '#1a212b', borderColor: '#2a3444' } as const

const VERDICT_META: Record<CompatVerdict, { text: string; color: string }> = {
  pass: { text: '通过', color: 'success' },
  fail: { text: '不兼容', color: 'error' },
  unknown: { text: '未登记', color: 'default' },
}

/** 任务是否还有可取消的节点 */
function hasCancellable(t: DeploymentOut): boolean {
  return t.nodes.some((n) => !DEPLOY_TERMINAL_NODE_STATES.includes(n.status))
}

/** 部署任务页：创建分发任务、下发前兼容性预检、跟踪进度、取消、失败重投。 */
export default function Deployments() {
  const [tasks, setTasks] = useState<DeploymentOut[]>([])
  const [loading, setLoading] = useState(true)
  const [open, setOpen] = useState(false)
  const [models, setModels] = useState<ModelOut[]>([])
  const [nodes, setNodes] = useState<NodeInfo[]>([])
  const [modelId, setModelId] = useState<string>()
  const [nodeIds, setNodeIds] = useState<string[]>([])
  const [note, setNote] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [prechecking, setPrechecking] = useState(false)
  const [precheck, setPrecheck] = useState<PrecheckNodeResult[] | null>(null)
  const [busyTask, setBusyTask] = useState('')

  const load = useCallback(async (showSpin = false) => {
    if (showSpin) setLoading(true)
    try {
      const data = await api.listDeployments()
      setTasks(data.items)
    } catch {
      /* 保留上次：轮询失败不该把界面清空 */
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load(true)
    const t = window.setInterval(() => load(false), 4000)
    return () => window.clearInterval(t)
  }, [load])

  const openCreate = async () => {
    setOpen(true)
    setModelId(undefined)
    setNodeIds([])
    setNote('')
    setPrecheck(null)
    try {
      const [m, n] = await Promise.all([api.listModels(), api.listNodes()])
      setModels(m.items)
      // 只列在线节点：给离线节点建任务，最后一定是一排"节点离线"，没有意义
      setNodes(n.items.filter((x) => x.status === 'online'))
    } catch (e) {
      message.error(errText(e, '无法获取模型/节点列表'))
    }
  }

  /**
   * 预检。**在用户点下发之前**调，把不兼容的节点先标出来 ——
   * 让选择阶段就暴露问题，而不是点完下发再看一片红。
   */
  const runPrecheck = useCallback(
    async (mid: string, nids: string[]) => {
      if (!mid || nids.length === 0) {
        setPrecheck(null)
        return
      }
      setPrechecking(true)
      try {
        const out = await api.precheckDeployment({ model_id: mid, node_ids: nids })
        setPrecheck(out.results)
      } catch {
        // 预检本身失败不该挡住下发（后端下发时还会再判一次），静默即可
        setPrecheck(null)
      } finally {
        setPrechecking(false)
      }
    },
    [],
  )

  const submit = async () => {
    if (!modelId) {
      message.warning('请选择模型')
      return
    }
    if (nodeIds.length === 0) {
      message.warning('请至少选择一个节点')
      return
    }
    const blocked = (precheck ?? []).filter((r) => !r.ok)
    if (blocked.length) {
      message.warning(
        `有 ${blocked.length} 个节点会被兼容性检查拦下（${blocked
          .map((b) => b.node_name || b.node_id)
          .join('、')}），仍要下发吗？下发后它们会直接标记失败。`,
        6,
      )
    }
    setSubmitting(true)
    try {
      await api.createDeployment({ model_id: modelId, node_ids: nodeIds, note: note.trim() })
      message.success('部署任务已创建并下发')
      setOpen(false)
      load(false)
    } catch (e) {
      message.error(errText(e, '创建失败'))
    } finally {
      setSubmitting(false)
    }
  }

  const retry = async (taskId: string) => {
    setBusyTask(taskId)
    try {
      await api.retryDeployment(taskId)
      message.success('已重投失败节点')
      load(false)
    } catch (e) {
      message.error(errText(e, '重试失败'))
    } finally {
      setBusyTask('')
    }
  }

  const cancel = async (taskId: string) => {
    setBusyTask(taskId)
    try {
      const out = await api.cancelDeployment(taskId)
      const missed = Object.entries(out.notified ?? {})
        .filter(([, ok]) => !ok)
        .map(([nid]) => nid)
      message.success(
        missed.length
          ? `已取消。${missed.length} 个节点离线，取消通知未送达（平台侧状态已生效）`
          : '已取消，并已通知设备停止下载',
      )
      load(false)
    } catch (e) {
      message.error(errText(e, '取消失败'))
    } finally {
      setBusyTask('')
    }
  }

  const remove = async (taskId: string) => {
    setBusyTask(taskId)
    try {
      await api.deleteDeployment(taskId)
      message.success('已删除该部署记录')
      load(false)
    } catch (e) {
      message.error(errText(e, '删除失败'))
    } finally {
      setBusyTask('')
    }
  }

  const columns = [
    {
      title: '模型',
      dataIndex: 'model_name',
      key: 'model',
      render: (v: string, r: DeploymentOut) => (
        <Space direction="vertical" size={0}>
          <span>{v}</span>
          <Typography.Text type="secondary" className="mono" style={{ fontSize: 11 }}>
            {r.model_id}
          </Typography.Text>
        </Space>
      ),
    },
    {
      title: '状态',
      dataIndex: 'status',
      key: 'status',
      width: 100,
      render: (s: string) => {
        const m = DEPLOY_TASK_STATUS_META[s as keyof typeof DEPLOY_TASK_STATUS_META] ?? {
          text: s,
          color: 'default',
        }
        return <Tag color={m.color}>{m.text}</Tag>
      },
    },
    {
      title: '目标',
      dataIndex: 'target_count',
      key: 'cnt',
      width: 70,
      render: (v: number) => `${v} 个`,
    },
    {
      title: '各节点进度',
      key: 'nodes',
      render: (_: unknown, r: DeploymentOut) => (
        <Space direction="vertical" size={4} style={{ width: '100%' }}>
          {r.nodes.map((n: DeployNodeOut) => {
            const m = DEPLOY_NODE_STATUS_META[n.status] ?? { text: n.status, color: 'default' }
            const downloaded = humanBytes(n.bytes_downloaded)
            const total = n.bytes_total ? ` / ${humanBytes(n.bytes_total)}` : ''
            const failed = n.status === 'failed'
            const done = n.status === 'ready'
            return (
              <div key={n.node_id} style={{ width: '100%' }}>
                <Space size={6} style={{ fontSize: 12 }} wrap>
                  <span>{n.node_name}</span>
                  <Tag color={m.color} style={{ marginInlineEnd: 0 }}>
                    {m.text}
                  </Tag>
                  {n.retry_count > 0 && (
                    <Typography.Text type="secondary" style={{ fontSize: 11 }}>
                      重试 {n.retry_count} 次
                    </Typography.Text>
                  )}
                  {n.error && (
                    <Typography.Text type="danger" style={{ fontSize: 11 }}>
                      {n.error}
                    </Typography.Text>
                  )}
                  {/* 回滚结论（A1）。它回答的是"我手机上现在是什么样"，
                      比"为什么失败"更值得看一眼 —— 所以单独一个 Tag，
                      不塞进上面那串红色的错误文字里被淹没。 */}
                  {n.rollback_note && (
                    <Tag
                      color={n.rollback_note.startsWith('还原') || n.rollback_note.startsWith('无需') ? 'blue' : 'warning'}
                      style={{ marginInlineEnd: 0, fontSize: 11 }}
                    >
                      {n.rollback_note}
                    </Tag>
                  )}
                </Space>
                <Progress
                  percent={n.progress}
                  size="small"
                  status={failed ? 'exception' : done ? 'success' : 'active'}
                  format={(p) => `${p}% (${downloaded}${total})`}
                />
              </div>
            )
          })}
        </Space>
      ),
    },
    {
      title: '创建时间',
      dataIndex: 'created_at',
      key: 'created',
      width: 165,
      render: (v: string | null) => (v ? formatDateTime(v) : '—'),
    },
    {
      title: '操作',
      key: 'action',
      width: 150,
      render: (_: unknown, r: DeploymentOut) => {
        const canCancel = hasCancellable(r)
        const canRetry = r.nodes.some((n) => n.status === 'failed')
        return (
          <Space size={12}>
            {canRetry && (
              <a onClick={() => (busyTask ? undefined : retry(r.task_id))}>重投失败</a>
            )}
            {canCancel && (
              <Popconfirm
                title="取消这次部署？"
                description="还没就绪的节点会立即标记为已取消，已就绪的节点不受影响。"
                onConfirm={() => cancel(r.task_id)}
                okText="取消部署"
                cancelText="返回"
              >
                <a style={{ color: '#faad14' }}>取消</a>
              </Popconfirm>
            )}
            <Popconfirm
              title="删除这条部署记录？"
              description={
                canCancel
                  ? '任务仍在进行，删除会先取消未完成的节点，再清除这条记录。'
                  : '仅清除列表里的历史记录，不影响已部署到设备上的模型。'
              }
              onConfirm={() => (busyTask ? undefined : remove(r.task_id))}
              okText="删除"
              cancelText="返回"
            >
              <a style={{ color: '#ff7875' }}>删除</a>
            </Popconfirm>
          </Space>
        )
      },
    },
  ]

  const blockedCount = (precheck ?? []).filter((r) => !r.ok).length

  return (
    <Card
      title="部署任务"
      size="small"
      style={CARD_STYLE}
      extra={
        <Button type="primary" onClick={openCreate}>
          新建部署
        </Button>
      }
    >
      <Table
        rowKey="task_id"
        size="small"
        loading={loading && tasks.length === 0}
        columns={columns as never}
        dataSource={tasks}
        pagination={{ pageSize: 10 }}
      />

      <Modal
        title="新建部署任务"
        open={open}
        onCancel={() => setOpen(false)}
        onOk={submit}
        confirmLoading={submitting}
        okText="创建并下发"
        cancelText="取消"
        width={680}
      >
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          <div>
            <div style={{ marginBottom: 4 }}>模型 *</div>
            <Select
              style={{ width: '100%' }}
              placeholder={models.length ? '选择模型' : '无可用模型'}
              value={modelId}
              onChange={(v) => {
                setModelId(v)
                runPrecheck(v, nodeIds)
              }}
              options={models.map((m) => ({
                value: m.model_id,
                label: `${m.name} (${m.version})${m.quantization ? ` · ${m.quantization}` : ''}${
                  m.target_soc ? ` · ${m.target_soc}` : ''
                }`,
              }))}
            />
          </div>
          <div>
            <div style={{ marginBottom: 4 }}>目标节点 *（仅在线节点）</div>
            <Select
              mode="multiple"
              style={{ width: '100%' }}
              placeholder={nodes.length ? '选择节点' : '无在线节点'}
              value={nodeIds}
              onChange={(v) => {
                setNodeIds(v)
                if (modelId) runPrecheck(modelId, v)
              }}
              options={nodes.map((n) => ({
                value: n.node_id,
                label: `${n.name || n.node_id}（${n.soc || 'SoC 未知'}）`,
              }))}
            />
          </div>
          <div>
            <div style={{ marginBottom: 4 }}>备注</div>
            <Input value={note} onChange={(e) => setNote(e.target.value)} placeholder="如：灰度第一批" />
          </div>

          {prechecking && (
            <Alert type="info" showIcon message="正在做兼容性预检…" />
          )}
          {!prechecking && precheck && (
            <Alert
              type={blockedCount ? 'warning' : 'success'}
              showIcon
              message={
                blockedCount
                  ? `有 ${blockedCount} 个节点会被拦下（下发后直接标记失败）`
                  : '全部节点兼容性检查通过'
              }
              description={
                <Space direction="vertical" size={6} style={{ width: '100%' }}>
                  {precheck.map((r) => (
                    <div key={r.node_id}>
                      <Space size={6} wrap>
                        <strong style={{ fontSize: 12 }}>{r.node_name || r.node_id}</strong>
                        <Tag color={r.ok ? 'success' : 'error'} style={{ marginInlineEnd: 0 }}>
                          {r.ok ? '可下发' : '不兼容'}
                        </Tag>
                        {r.reason && (
                          <Typography.Text type={r.ok ? 'secondary' : 'danger'} style={{ fontSize: 12 }}>
                            {r.reason}
                          </Typography.Text>
                        )}
                      </Space>
                      {(r.checks ?? []).some((c: CompatCheck) => c.verdict !== 'pass') && (
                        <div style={{ fontSize: 11, color: '#8c8c8c', paddingLeft: 2 }}>
                          {(r.checks ?? []).map((c: CompatCheck) => (
                            <span key={c.name} style={{ marginRight: 10 }}>
                              {c.name}：
                              <Tag
                                color={VERDICT_META[c.verdict]?.color ?? 'default'}
                                style={{ marginInlineEnd: 4, transform: 'scale(0.9)' }}
                              >
                                {VERDICT_META[c.verdict]?.text ?? c.verdict}
                              </Tag>
                            </span>
                          ))}
                        </div>
                      )}
                    </div>
                  ))}
                </Space>
              }
            />
          )}
        </Space>
      </Modal>
    </Card>
  )
}
