import { useCallback, useEffect, useMemo, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import {
  Alert,
  Button,
  Card,
  Col,
  Descriptions,
  Empty,
  Progress,
  Row,
  Segmented,
  Space,
  Spin,
  Table,
  Tag,
  Typography,
} from 'antd'
import { ArrowLeftOutlined } from '@ant-design/icons'
import { api } from '../api'
import MetricChart from '../components/MetricChart'
import { HeartbeatText, NodeStatusTag, ServiceStatusTag } from '../components/NodeStatusTag'
import { humanBytes, humanDuration, parseUtc, ratio } from '../utils/format'
import { METRIC_COLORS, MONO_FONT } from '../utils/theme'
import type { MetricSample, NodeInfo } from '../types'

interface ServiceRow {
  service_id: string
  node_id: string
  model_id: string | null
  name: string
  status: 'stopped' | 'starting' | 'running' | 'error'
  port: number
  endpoint: string
  api_type: string
  pid: number | null
  started_at: string | null
  last_error: string
}

/** 采样新鲜度阈值（秒）。与后端 metrics_query.STALE_FACTOR × 心跳周期一致。 */
const STALE_SECONDS = 15

/** 单个指标的展示块。value 为 null 时显式显示「—」，即"拿不到"，不是 0。 */
function MetricCell({
  label,
  value,
  color,
  percent,
}: {
  label: string
  value: number | null
  color: string
  percent?: number | null
}) {
  const unavailable = value === null || value === undefined
  return (
    <Card size="small" style={{ background: '#1a212b', borderColor: '#2a3444', height: '100%' }}>
      <div style={{ fontSize: 12, color: '#8c8c8c', marginBottom: 2 }}>{label}</div>
      <div
        style={{
          fontSize: 20,
          fontWeight: 600,
          fontFamily: MONO_FONT,
          color: unavailable ? '#5a5a5a' : color,
          lineHeight: '28px',
        }}
      >
        {unavailable ? '—' : value.toFixed(1)}
        {!unavailable && <span style={{ fontSize: 12, marginLeft: 2, color: '#8c8c8c' }}>%</span>}
      </div>
      {percent !== undefined && (
        <Progress
          percent={percent ?? 0}
          showInfo={false}
          strokeColor={unavailable ? '#2a3444' : color}
          trailColor="#2a3444"
          size="small"
          style={{ marginBottom: 0 }}
        />
      )}
    </Card>
  )
}

/**
 * 节点详情页（里程碑 2）。
 *
 * 关键行为：节点离线时，**当前指标一律显示「—」并给出明确提示**，
 * 但历史曲线照常渲染（历史数据本身是真实发生过的，可以看）。
 * 这样既不会"用旧值冒充当前值"，也不丢历史信息。
 */
export default function NodeDetail() {
  const { nodeId = '' } = useParams()
  const navigate = useNavigate()
  const [node, setNode] = useState<NodeInfo | null>(null)
  const [services, setServices] = useState<ServiceRow[]>([])
  const [samples, setSamples] = useState<MetricSample[]>([])
  const [range, setRange] = useState(30)
  const [loading, setLoading] = useState(true)
  const [notFound, setNotFound] = useState(false)

  const loadNode = useCallback(async () => {
    try {
      const [n, s] = await Promise.all([api.getNode(nodeId), api.nodeServices(nodeId)])
      setNode(n)
      setServices((s as { items: ServiceRow[] }).items ?? [])
      setNotFound(false)
    } catch (e: unknown) {
      const status = (e as { response?: { status?: number } })?.response?.status
      if (status === 404) setNotFound(true)
    } finally {
      setLoading(false)
    }
  }, [nodeId])

  const loadMetrics = useCallback(
    async (minutes: number) => {
      try {
        const d = await api.nodeMetrics(nodeId, minutes)
        setSamples(d.items)
      } catch {
        /* 指标拉取失败不影响基础信息展示 */
      }
    },
    [nodeId],
  )

  useEffect(() => {
    setLoading(true)
    loadNode()
    const t = window.setInterval(loadNode, 5000)
    return () => window.clearInterval(t)
  }, [loadNode])

  useEffect(() => {
    loadMetrics(range)
    const t = window.setInterval(() => loadMetrics(range), 10000)
    return () => window.clearInterval(t)
  }, [loadMetrics, range])

  // ---- 当前指标：仅在"在线 且 采样新鲜"时才可展示 ----
  const last = samples.length > 0 ? samples[samples.length - 1] : null
  const lastTs = parseUtc(last?.ts)?.getTime() ?? null
  const ageSeconds = lastTs ? Math.max(0, Math.floor((Date.now() - lastTs) / 1000)) : null
  const isOnline = node?.status === 'online'
  const isFresh = ageSeconds !== null && ageSeconds <= STALE_SECONDS
  /** 当前指标是否可以信任（离线 / 采样陈旧一律不可信） */
  const live = Boolean(isOnline && isFresh)
  const m = live ? last : null

  // ---- 曲线数据 ----
  const chart = useMemo(() => {
    const ts = samples.map((s) => s.ts)
    return {
      timestamps: ts,
      usage: [
        { name: 'CPU', color: METRIC_COLORS.cpu, data: samples.map((s) => s.cpu_usage), unit: '%' },
        { name: 'GPU', color: METRIC_COLORS.gpu, data: samples.map((s) => s.gpu_usage), unit: '%' },
        { name: 'NPU', color: METRIC_COLORS.npu, data: samples.map((s) => s.npu_usage), unit: '%' },
      ],
      capacity: [
        {
          name: '内存使用率',
          color: METRIC_COLORS.mem,
          data: samples.map((s) => ratio(s.mem_used, s.mem_total)),
          unit: '%',
        },
        {
          name: '存储使用率',
          color: METRIC_COLORS.storage,
          data: samples.map((s) => ratio(s.storage_used, s.storage_total)),
          unit: '%',
        },
      ],
      temp: [
        {
          name: '温度',
          color: METRIC_COLORS.temp,
          data: samples.map((s) => s.temperature),
          unit: '℃',
          area: true,
        },
      ],
      net: [
        {
          name: '下行',
          color: METRIC_COLORS.netRx,
          data: toRate(ts, samples.map((s) => s.net_rx_bytes)),
          unit: '/s',
          area: true,
        },
        {
          name: '上行',
          color: METRIC_COLORS.netTx,
          data: toRate(ts, samples.map((s) => s.net_tx_bytes)),
          unit: '/s',
        },
      ],
    }
  }, [samples])

  if (loading && !node) {
    return (
      <div style={{ textAlign: 'center', padding: 60 }}>
        <Spin />
      </div>
    )
  }

  if (notFound || !node) {
    return (
      <Card style={{ background: '#1a212b', borderColor: '#2a3444' }}>
        <Empty description={`节点不存在：${nodeId}`}>
          <Button onClick={() => navigate('/nodes')}>返回节点列表</Button>
        </Empty>
      </Card>
    )
  }

  return (
    <Space direction="vertical" size={16} style={{ width: '100%' }}>
      <Space>
        <Button icon={<ArrowLeftOutlined />} onClick={() => navigate('/nodes')}>
          返回
        </Button>
        <Typography.Title level={4} style={{ margin: 0 }}>
          {node.name || node.node_id}
        </Typography.Title>
        <NodeStatusTag status={node.status} />
        {node.is_simulated && <Tag color="blue">模拟节点</Tag>}
        {live && ageSeconds !== null && (
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            指标 {ageSeconds} 秒前
          </Typography.Text>
        )}
      </Space>

      {/* 离线 / 陈旧时明确说明：下面的当前值不可信 */}
      {!isOnline && (
        <Alert
          type={node.status === 'offline' ? 'error' : 'info'}
          showIcon
          message={node.status === 'offline' ? '节点已离线' : '节点尚未接入'}
          description={
            node.status === 'offline'
              ? '该节点心跳已超时，当前指标不参与展示与集群汇总。下方历史曲线仍为真实发生过的采样，可以查看。'
              : '该节点从未上报过心跳，暂无任何指标数据。'
          }
        />
      )}
      {isOnline && !isFresh && (
        <Alert
          type="warning"
          showIcon
          message="指标数据已陈旧"
          description={`最后一次采样在 ${ageSeconds ?? '?'} 秒前（超过 ${STALE_SECONDS}s 阈值）。心跳仍在，但采集可能卡住，因此不把最后一次旧值当作当前值展示。`}
        />
      )}

      <Card title="基础信息" size="small" style={{ background: '#1a212b', borderColor: '#2a3444' }}>
        <Descriptions
          column={3}
          size="small"
          labelStyle={{ color: '#8c8c8c' }}
          contentStyle={{ color: '#e6edf3' }}
          items={[
            { key: 'id', label: '节点编号', children: <span className="mono">{node.node_id}</span> },
            { key: 'model', label: '型号', children: node.model || '—' },
            { key: 'soc', label: 'SoC', children: node.soc || '—' },
            { key: 'android', label: 'Android 版本', children: node.android_version || '—' },
            {
              key: 'agent',
              label: 'Agent 版本',
              children: <span className="mono">{node.agent_version || '—'}</span>,
            },
            { key: 'ip', label: 'IP 地址', children: <span className="mono">{node.ip || '—'}</span> },
            {
              key: 'hb',
              label: '最后心跳',
              children: <HeartbeatText lastHeartbeat={node.last_heartbeat} status={node.status} />,
            },
            {
              key: 'uptime',
              label: '在线时长',
              children: node.status === 'online' ? humanDuration(node.uptime_seconds) : '—',
            },
            {
              key: 'extra',
              label: '扩展信息',
              children: m?.extra ? (
                <span className="mono" style={{ fontSize: 11 }}>
                  {formatExtra(m.extra)}
                </span>
              ) : (
                '—'
              ),
            },
          ]}
        />
      </Card>

      {/* ---------- 当前指标 ---------- */}
      <Card
        title="当前指标"
        size="small"
        style={{ background: '#1a212b', borderColor: '#2a3444' }}
        extra={
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            {live ? '实时（每 5 秒上报）' : '不可用 —— 节点离线或数据陈旧'}
          </Typography.Text>
        }
      >
        <Row gutter={[12, 12]}>
          <Col xs={12} sm={8} lg={4}>
            <MetricCell label="CPU" value={m?.cpu_usage ?? null} color={METRIC_COLORS.cpu} />
          </Col>
          <Col xs={12} sm={8} lg={4}>
            <MetricCell label="GPU" value={m?.gpu_usage ?? null} color={METRIC_COLORS.gpu} />
          </Col>
          <Col xs={12} sm={8} lg={4}>
            <MetricCell label="NPU" value={m?.npu_usage ?? null} color={METRIC_COLORS.npu} />
          </Col>
          <Col xs={12} sm={8} lg={4}>
            <MetricCell
              label="内存"
              value={m ? ratio(m.mem_used, m.mem_total) : null}
              color={METRIC_COLORS.mem}
              percent={m ? ratio(m.mem_used, m.mem_total) : null}
            />
          </Col>
          <Col xs={12} sm={8} lg={4}>
            <MetricCell
              label="存储"
              value={m ? ratio(m.storage_used, m.storage_total) : null}
              color={METRIC_COLORS.storage}
              percent={m ? ratio(m.storage_used, m.storage_total) : null}
            />
          </Col>
          <Col xs={12} sm={8} lg={4}>
            <Card size="small" style={{ background: '#1a212b', borderColor: '#2a3444', height: '100%' }}>
              <div style={{ fontSize: 12, color: '#8c8c8c', marginBottom: 2 }}>温度 / 功耗</div>
              <div
                style={{
                  fontSize: 20,
                  fontWeight: 600,
                  fontFamily: MONO_FONT,
                  color: m?.temperature == null ? '#5a5a5a' : METRIC_COLORS.temp,
                  lineHeight: '28px',
                }}
              >
                {m?.temperature == null ? '—' : `${m.temperature.toFixed(1)}℃`}
              </div>
              <div style={{ fontSize: 11, color: '#5a5a5a' }}>
                功耗 {m?.power_watts == null ? '未提供' : `${m.power_watts} W`}
              </div>
            </Card>
          </Col>
          <Col xs={24} lg={12}>
            <Card size="small" style={{ background: '#1a212b', borderColor: '#2a3444', height: '100%' }}>
              <div style={{ fontSize: 12, color: '#8c8c8c', marginBottom: 4 }}>内存 / 存储容量</div>
              <Space direction="vertical" size={2} style={{ width: '100%' }}>
                <span className="mono" style={{ fontSize: 13 }}>
                  内存 {humanBytes(m?.mem_used)} / {humanBytes(m?.mem_total)}
                </span>
                <span className="mono" style={{ fontSize: 13 }}>
                  存储 {humanBytes(m?.storage_used)} / {humanBytes(m?.storage_total)}
                </span>
              </Space>
            </Card>
          </Col>
          <Col xs={24} lg={12}>
            <Card size="small" style={{ background: '#1a212b', borderColor: '#2a3444', height: '100%' }}>
              <div style={{ fontSize: 12, color: '#8c8c8c', marginBottom: 4 }}>网络累计 / 负载</div>
              <Space direction="vertical" size={2} style={{ width: '100%' }}>
                <span className="mono" style={{ fontSize: 13 }}>
                  ↓ {humanBytes(m?.net_rx_bytes)} ↑ {humanBytes(m?.net_tx_bytes)}
                </span>
                <span className="mono" style={{ fontSize: 13 }}>
                  负载 {m?.load_avg ?? '—'}
                </span>
              </Space>
            </Card>
          </Col>
        </Row>
      </Card>

      {/* ---------- 历史曲线 ---------- */}
      <Card
        title="指标历史曲线"
        size="small"
        style={{ background: '#1a212b', borderColor: '#2a3444' }}
        extra={
          <Space size={8}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              {samples.length} 个采样点
            </Typography.Text>
            <Segmented
              size="small"
              value={range}
              onChange={(v) => setRange(v as number)}
              options={[
                { label: '15 分钟', value: 15 },
                { label: '1 小时', value: 60 },
                { label: '6 小时', value: 360 },
                { label: '24 小时', value: 1440 },
              ]}
            />
          </Space>
        }
      >
        <Space direction="vertical" size={14} style={{ width: '100%' }}>
          <div>
            <Typography.Text style={{ fontSize: 12, color: '#8c8c8c' }}>
              处理器使用率
            </Typography.Text>
            <MetricChart
              timestamps={chart.timestamps}
              series={chart.usage}
              height={220}
              yMax={100}
              valueFormatter={(v) => v.toFixed(0)}
              emptyText="暂无采样数据"
            />
          </div>
          <div>
            <Typography.Text style={{ fontSize: 12, color: '#8c8c8c' }}>容量使用率</Typography.Text>
            <MetricChart
              timestamps={chart.timestamps}
              series={chart.capacity}
              height={200}
              yMax={100}
              valueFormatter={(v) => v.toFixed(0)}
              emptyText="暂无采样数据"
            />
          </div>
          <Row gutter={[14, 14]}>
            <Col xs={24} lg={12}>
              <Typography.Text style={{ fontSize: 12, color: '#8c8c8c' }}>温度（℃）</Typography.Text>
              <MetricChart
                timestamps={chart.timestamps}
                series={chart.temp}
                height={190}
                valueFormatter={(v) => v.toFixed(0)}
                emptyText="设备未暴露温度，无数据"
                showLegend={false}
              />
            </Col>
            <Col xs={24} lg={12}>
              <Typography.Text style={{ fontSize: 12, color: '#8c8c8c' }}>
                网络速率（由累计字节差分）
              </Typography.Text>
              <MetricChart
                timestamps={chart.timestamps}
                series={chart.net}
                height={190}
                valueFormatter={(v) => humanBytes(v)}
                emptyText="暂无采样数据"
              />
            </Col>
          </Row>
          <Typography.Text type="secondary" style={{ fontSize: 11 }}>
            曲线断点表示该时刻采集不到该项（例如手机不暴露 NPU/GPU 使用率），表示"无数据"而非数值为 0。
          </Typography.Text>
        </Space>
      </Card>

      {/* ---------- 服务列表 ---------- */}
      <Card
        title="运行中的模型服务"
        size="small"
        style={{ background: '#1a212b', borderColor: '#2a3444' }}
        extra={
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            服务管理功能将在里程碑 4 提供
          </Typography.Text>
        }
      >
        {services.length === 0 ? (
          <Empty description="该节点当前没有模型服务" image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : (
          <Table
            rowKey="service_id"
            size="small"
            pagination={false}
            dataSource={services}
            columns={[
              { title: '服务名', dataIndex: 'name', key: 'name' },
              {
                title: '状态',
                dataIndex: 'status',
                key: 'status',
                width: 100,
                render: (s: ServiceRow['status']) => <ServiceStatusTag status={s} />,
              },
              { title: '端口', dataIndex: 'port', key: 'port', width: 90 },
              {
                title: 'Endpoint',
                dataIndex: 'endpoint',
                key: 'endpoint',
                render: (v: string) => (
                  <span className="mono" style={{ fontSize: 12 }}>
                    {v || '—'}
                  </span>
                ),
              },
              { title: 'API 类型', dataIndex: 'api_type', key: 'api_type', width: 100 },
            ] as never}
          />
        )}
      </Card>
    </Space>
  )
}

/**
 * 累计字节 → 速率（bytes/s）。
 *
 * 需要相邻两点的差值与时间间隔；计数器回绕或时间倒序时返回 null（断点），
 * 不硬算出个负值或天文数字。
 */
function toRate(timestamps: string[], cumulative: (number | null)[]): (number | null)[] {
  const out: (number | null)[] = []
  for (let i = 0; i < cumulative.length; i += 1) {
    if (i === 0) {
      out.push(null)
      continue
    }
    const a = cumulative[i - 1]
    const b = cumulative[i]
    const t1 = parseUtc(timestamps[i - 1])?.getTime()
    const t2 = parseUtc(timestamps[i])?.getTime()
    if (a === null || b === null || b < a || !t1 || !t2 || t2 <= t1) {
      out.push(null)
      continue
    }
    out.push((b - a) / ((t2 - t1) / 1000))
  }
  return out
}

/** extra JSON → 简短可读文本 */
function formatExtra(extra: Record<string, unknown>): string {
  const parts: string[] = []
  if (extra.cpu_freq_avg_mhz) parts.push(`${Number(extra.cpu_freq_avg_mhz).toFixed(0)}MHz`)
  if (extra.cpu_cores) parts.push(`${extra.cpu_cores}核`)
  if (extra.battery_percent !== undefined) parts.push(`电量 ${extra.battery_percent}%`)
  if (extra.battery_status) parts.push(String(extra.battery_status))
  if (extra.simulated) parts.push('模拟数据')
  return parts.length ? parts.join(' · ') : '—'
}
