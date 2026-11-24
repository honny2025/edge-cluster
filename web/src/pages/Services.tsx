import { useCallback, useEffect, useState } from 'react'
import {
  Badge,
  Button,
  Card,
  Collapse,
  Form,
  Input,
  InputNumber,
  Modal,
  Popconfirm,
  Select,
  Space,
  Table,
  Tag,
  Typography,
  message,
} from 'antd'
import { api, errText } from '../api'
import { SERVICE_STATUS_META, API_TYPE_META } from '../types'
import type { ApiType, ModelOut, NodeInfo, ServiceOut } from '../types'
import { formatDateTime } from '../utils/format'

/** 这些状态下进程还在（或正在变动），必须先停才能删 */
const ACTIVE_STATES: ServiceOut['status'][] = ['starting', 'running', 'restarting']

/** 服务管理页：创建托管服务，启停/重启，查看日志、主动探活、网关统计。 */
export default function Services() {
  const [services, setServices] = useState<ServiceOut[]>([])
  const [loading, setLoading] = useState(true)
  const [open, setOpen] = useState(false)
  const [nodes, setNodes] = useState<NodeInfo[]>([])
  const [models, setModels] = useState<ModelOut[]>([])
  const [submitting, setSubmitting] = useState(false)

  const [logOpen, setLogOpen] = useState(false)
  const [logLines, setLogLines] = useState<string[]>([])
  const [logLoading, setLogLoading] = useState(false)
  const [logTitle, setLogTitle] = useState('')

  const load = useCallback(async (showSpin = false) => {
    if (showSpin) setLoading(true)
    try {
      setServices(await api.listServices())
    } catch {
      /* 保留上次 */
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
    try {
      const [n, m] = await Promise.all([api.listNodes(), api.listModels()])
      setNodes(n.items.filter((x) => x.status === 'online'))
      setModels(m.items)
    } catch {
      message.error('无法获取节点/模型列表')
    }
  }

  const onCreate = async (values: Record<string, unknown>) => {
    setSubmitting(true)
    try {
      const argsRaw = (values.args as string) || ''
      const args = argsRaw
        .split('\n')
        .map((s) => s.trim())
        .filter(Boolean)
      await api.createService({
        // 表单值是 unknown：antd 的 Form 不做泛型推导，这里显式收窄
        node_id: String(values.node_id ?? ''),
        model_id: values.model_id ? String(values.model_id) : null,
        name: String(values.name ?? ''),
        port: Number(values.port) || 0,
        api_type: (values.api_type as ApiType) ?? 'other',
        command: String(values.command ?? ''),
        args,
        health_url: String(values.health_url ?? ''),
        auto_restart: Boolean(values.auto_restart),
        // 需求 §7：设备/Agent 重启后是否自动恢复。默认关闭 ——
        // 默认替用户决定，等于给了他一个不受控的自启动项。
        auto_start: Boolean(values.auto_start),
        mem_required_mb: Number(values.mem_required_mb) || 0,
        mutex_group: String(values.mutex_group ?? ''),
      })
      message.success('服务已创建（初始为已停止，请在列表里启动）')
      setOpen(false)
      load(false)
    } catch (e) {
      message.error(errText(e, '创建失败'))
    } finally {
      setSubmitting(false)
    }
  }

  const doAction = async (id: string, action: 'start' | 'stop' | 'restart') => {
    try {
      await api.serviceAction(id, action)
      message.success(`已下发 ${action}`)
      load(false)
    } catch (e) {
      // 后端 409 的 detail 是给人看的原因（"资源准入失败：可用内存 300MB < 需求 800MB"），
      // 直接展示它，比前端编一句"操作失败"有用得多。
      message.error(errText(e, '操作失败'))
    }
  }

  /**
   * 删除服务定义。
   *
   * 运行中的服务后端会回 409 —— 这不是限制，是保护：直接删掉一条"正在跑"的记录，
   * 设备上那个进程就成了平台看不见的孤儿（还占着端口和内存）。
   * 所以这里**不替用户先停再删**，而是把后端的话原样告诉他，让他自己决定。
   */
  const removeService = async (svc: ServiceOut) => {
    try {
      const out = await api.deleteService(svc.service_id)
      message.success(out.agent_notified ? '已删除' : '已删除（节点离线，设备侧残留由看门狗清理）')
      load(false)
    } catch (e) {
      message.error(errText(e, '删除失败'))
    }
  }

  const viewLogs = async (svc: ServiceOut) => {
    setLogOpen(true)
    setLogTitle(`${svc.name} 日志`)
    setLogLoading(true)
    setLogLines([])
    try {
      const data = await api.serviceLogs(svc.service_id)
      setLogLines(data.lines || [])
      if (data.note) message.info(data.note)
    } catch {
      message.error('获取日志失败，节点可能离线')
    } finally {
      setLogLoading(false)
    }
  }

  const checkHealth = async (svc: ServiceOut) => {
    try {
      const data = await api.serviceCheck(svc.service_id)
      const ok = data.probe?.ok
      if (ok) message.success(`探活成功：${data.probe.detail}`)
      else message.warning(`探活失败：${data.probe?.detail}`)
      load(false)
    } catch (e: unknown) {
      const detail = (e as { response?: { data?: { detail?: string } } })?.response?.data?.detail
      message.error(errText(e, '探活失败'))
    }
  }

  const columns = [
    {
      title: '名称',
      dataIndex: 'name',
      key: 'name',
      render: (_: string, r: ServiceOut) => (
        <Space direction="vertical" size={0}>
          <span>{r.name}</span>
          <Typography.Text type="secondary" className="mono" style={{ fontSize: 11 }}>
            {r.service_id}
          </Typography.Text>
        </Space>
      ),
    },
    {
      title: '状态',
      dataIndex: 'status',
      key: 'status',
      width: 110,
      render: (s: string) => {
        const m = SERVICE_STATUS_META[s as keyof typeof SERVICE_STATUS_META] ?? {
          text: s,
          color: 'default',
        }
        return <Badge status={m.color as never} text={m.text} />
      },
    },
    {
      title: '端口/类型',
      key: 'port',
      width: 130,
      render: (_: unknown, r: ServiceOut) => (
        <Space direction="vertical" size={0}>
          <span className="mono">:{r.port}</span>
          <Tag>{API_TYPE_META[r.api_type as ApiType]?.text ?? r.api_type}</Tag>
        </Space>
      ),
    },
    {
      title: '准入',
      key: 'admission',
      width: 160,
      render: (_: unknown, r: ServiceOut) => (
        <Space direction="vertical" size={0} style={{ fontSize: 12 }}>
          <span>{r.mem_required_mb ? `内存≥${r.mem_required_mb}MB` : '内存不限'}</span>
          <span>{r.mutex_group ? `互斥组:${r.mutex_group}` : '无互斥'}</span>
        </Space>
      ),
    },
    {
      title: '探活',
      key: 'probe',
      width: 120,
      render: (_: unknown, r: ServiceOut) => (
        <span style={{ fontSize: 12 }}>
          {r.health_url ? (
            r.probe_ok ? (
              <Typography.Text type="success">正常</Typography.Text>
            ) : (
              <Typography.Text type="danger">异常</Typography.Text>
            )
          ) : (
            <Typography.Text type="secondary">未配置</Typography.Text>
          )}
        </span>
      ),
    },
    {
      title: '自恢复',
      key: 'auto',
      width: 120,
      render: (_: unknown, r: ServiceOut) => (
        <Space direction="vertical" size={0} style={{ fontSize: 12 }}>
          <span>
            {r.auto_start ? (
              <Typography.Text type="success">开机自启</Typography.Text>
            ) : (
              <Typography.Text type="secondary">开机不自启</Typography.Text>
            )}
          </span>
          <span style={{ color: '#8c8c8c' }}>
            {r.auto_restart ? '崩溃自拉' : '崩溃不拉'}
            {r.restart_count ? ` · 重启 ${r.restart_count} 次` : ''}
          </span>
        </Space>
      ),
    },
    {
      title: '错误信息',
      dataIndex: 'last_error',
      key: 'err',
      render: (v: string, r: ServiceOut) =>
        v ? (
          <Typography.Text type="danger" style={{ fontSize: 12 }}>
            {v}
            {r.last_exit_code !== null && r.last_exit_code !== undefined
              ? `（退出码 ${r.last_exit_code}）`
              : ''}
          </Typography.Text>
        ) : (
          '—'
        ),
    },
    {
      title: '状态变更',
      dataIndex: 'state_changed_at',
      key: 'changed',
      width: 160,
      render: (v: string | null) => (v ? formatDateTime(v) : '—'),
    },
    {
      title: '操作',
      key: 'action',
      width: 260,
      render: (_: unknown, r: ServiceOut) => {
        const active = ACTIVE_STATES.includes(r.status)
        return (
          <Space size={4} wrap>
            <a onClick={() => doAction(r.service_id, 'start')}>启动</a>
            <a onClick={() => doAction(r.service_id, 'stop')}>停止</a>
            <a onClick={() => doAction(r.service_id, 'restart')}>重启</a>
            <a onClick={() => viewLogs(r)}>日志</a>
            <a onClick={() => checkHealth(r)}>探活</a>
            {active ? (
              // 运行中不给删除入口：点了必然 409。与其让它报错，
              // 不如在界面上就说清"要先停"。
              <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                先停止才能删
              </Typography.Text>
            ) : (
              <Popconfirm
                title="删除该服务定义？"
                description="只删除平台上的记录（并通知 Agent 丢掉本地副本），不会动模型文件。"
                onConfirm={() => removeService(r)}
                okText="删除"
                cancelText="取消"
              >
                <a style={{ color: '#ff7875' }}>删除</a>
              </Popconfirm>
            )}
          </Space>
        )
      },
    },
  ]

  return (
    <Card
      title="服务管理"
      size="small"
      style={{ background: '#1a212b', borderColor: '#2a3444' }}
      extra={
        <Button type="primary" onClick={openCreate}>
          新建服务
        </Button>
      }
    >
      <Table
        rowKey="service_id"
        size="small"
        loading={loading && services.length === 0}
        columns={columns as never}
        dataSource={services}
        pagination={{ pageSize: 10 }}
      />

      <Modal
        title="新建服务"
        open={open}
        onCancel={() => setOpen(false)}
        footer={null}
      >
        <Form layout="vertical" onFinish={onCreate} initialValues={{ api_type: 'other', auto_restart: true }}>
          <Typography.Paragraph type="secondary" style={{ fontSize: 12, marginBottom: 12 }}>
            最小只需：选节点 + 填服务名 + 填启动命令。端口、探活、内存、互斥等都可留空，需要时再展开「高级设置」。
          </Typography.Paragraph>
          <Form.Item name="node_id" label="节点 *" rules={[{ required: true, message: '请选择节点' }]}>
            <Select
              placeholder={nodes.length ? '选择节点' : '无在线节点'}
              options={nodes.map((n) => ({ value: n.node_id, label: n.name || n.node_id }))}
            />
          </Form.Item>
          <Form.Item name="model_id" label="关联模型（可选）">
            <Select
              allowClear
              placeholder="可选"
              options={models.map((m) => ({ value: m.model_id, label: `${m.name} (${m.version})` }))}
            />
          </Form.Item>
          <Form.Item name="name" label="服务名 *" rules={[{ required: true, message: '请填写名称' }]}>
            <Input placeholder="如 qwen-chat" />
          </Form.Item>
          <Form.Item
            name="command"
            label="启动命令 *"
            rules={[{ required: true, message: '请填写启动命令' }]}
            extra="填手机（Termux）里已装好的可执行文件名，如 python、python3、llama-server。参数填在下方「高级设置 → 启动参数」"
          >
            <Input placeholder="python" />
          </Form.Item>
          <Form.Item name="port" label="端口（可选）">
            <InputNumber min={0} max={65535} style={{ width: '100%' }} placeholder="8080" />
          </Form.Item>
          <Collapse
            ghost
            size="small"
            items={[
              {
                key: 'advanced',
                label: '高级设置（可选）',
                children: (
                  <>
                    <Form.Item name="api_type" label="API 类型">
                      <Select
                        options={Object.entries(API_TYPE_META).map(([v, m]) => ({
                          value: v,
                          label: m.text,
                        }))}
                      />
                    </Form.Item>
                    <Form.Item name="args" label="启动参数（每行一条）">
                      <Input.TextArea rows={2} placeholder={'-m http.server\n8080'} />
                    </Form.Item>
                    <Form.Item name="health_url" label="探活路径（可选）" extra="如 /health，用于主动探活">
                      <Input placeholder="/health" />
                    </Form.Item>
                    <Space size={12} style={{ display: 'flex' }}>
                      <Form.Item name="mem_required_mb" label="所需内存(MB,0=不限)" style={{ flex: 1 }}>
                        <InputNumber min={0} style={{ width: '100%' }} />
                      </Form.Item>
                      <Form.Item name="mutex_group" label="互斥组（可选）" style={{ flex: 1 }}>
                        <Input placeholder="如 llm-group" />
                      </Form.Item>
                    </Space>
                    <Space size={12} style={{ display: 'flex' }}>
                      <Form.Item
                        name="auto_restart"
                        label="崩溃后自动拉起"
                        style={{ flex: 1 }}
                        extra="进程自己死了，Agent 看门狗把它拉回来"
                      >
                        <Select
                          options={[
                            { value: true, label: '是' },
                            { value: false, label: '否' },
                          ]}
                        />
                      </Form.Item>
                      <Form.Item
                        name="auto_start"
                        label="设备/Agent 重启后自动恢复"
                        style={{ flex: 1 }}
                        extra="默认否 —— 重启后要不要自己起来，应由你明确决定"
                      >
                        <Select
                          options={[
                            { value: false, label: '否（默认）' },
                            { value: true, label: '是' },
                          ]}
                        />
                      </Form.Item>
                    </Space>
                  </>
                ),
              },
            ]}
          />
          <Space>
            <Button type="primary" htmlType="submit" loading={submitting}>
              创建
            </Button>
            <Button onClick={() => setOpen(false)}>取消</Button>
          </Space>
        </Form>
      </Modal>

      <Modal
        title={logTitle}
        open={logOpen}
        onCancel={() => setLogOpen(false)}
        footer={null}
        width={720}
      >
        <Typography.Paragraph
          style={{
            background: '#0d1117',
            padding: 12,
            borderRadius: 6,
            maxHeight: 420,
            overflow: 'auto',
            fontFamily: 'monospace',
            fontSize: 12,
            whiteSpace: 'pre-wrap',
          }}
        >
          {logLoading ? '加载中…' : (logLines.length ? logLines.join('\n') : '（无日志）')}
        </Typography.Paragraph>
      </Modal>
    </Card>
  )
}
