import { useCallback, useEffect, useMemo, useState } from 'react'
import { Alert, Badge, Card, Col, Empty, Progress, Row, Segmented, Space, Table, Tooltip, Typography } from 'antd'
import {
  ApiOutlined,
  CheckCircleOutlined,
  CloseCircleOutlined,
  DatabaseOutlined,
  DashboardOutlined,
  FireOutlined,
} from '@ant-design/icons'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'
import { NODE_EVENT } from '../App'
import MetricChart from '../components/MetricChart'
import { HeartbeatText, NodeStatusTag } from '../components/NodeStatusTag'
import { humanBytes, humanDuration, humanPercent, ratio } from '../utils/format'
import { METRIC_COLORS, MONO_FONT } from '../utils/theme'
import { ALERT_KIND_META } from '../types'
import type { ClusterSeriesOut, MetricSample, NodeInfo, OverviewOut } from '../types'

/**
 * 首页监控大屏（里程碑 2）。
 *
 * 数据纪律（任务书 §4 的核心要求，前端这侧同样要守）：
 *
 * - 离线节点在列表里明确显示「离线」，其指标列一律显示「—」。
 *   后端 `nodes_metrics` 对离线和陈旧采样直接给 null，前端拿不到旧值，
 *   结构上就不可能"拿旧数据冒充在线"。
 * - 指标为 null 显示「—」而不是 0。拿不到就是拿不到，不造假。
 *
 * 刷新策略：轮询 + WebSocket 事件双通道。曲线图关动画并以 merge 模式更新，
 * 保证持续刷新时画面稳定不闪烁。
 */

/** KPI 卡片。手写而非用 Statistic —— Statistic 的数字滚动动画在秒级刷新时显得闪。 */
function KpiCard({
  title,
  value,
  suffix,
  sub,
  color = '#e6edf3',
  icon,
  danger,
}: {
  title: string
  value: string | number
  suffix?: string
  sub?: string
  color?: string
  icon?: React.ReactNode
  danger?: boolean
}) {
  return (
    <Card
      size="small"
      style={{
        background: '#1a212b',
        borderColor: danger ? '#5c2b2f' : '#2a3444',
        height: '100%',
      }}
      styles={{ body: { padding: '12px 14px' } }}
    >
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <span style={{ fontSize: 12, color: '#8c8c8c' }}>{title}</span>
        {icon}
      </div>
      <div
        style={{
          fontSize: 26,
          fontWeight: 600,
          fontFamily: MONO_FONT,
          color,
          lineHeight: '36px',
          whiteSpace: 'nowrap',
        }}
      >
        {value}
        {suffix && <span style={{ fontSize: 13, marginLeft: 3, color: '#8c8c8c' }}>{suffix}</span>}
      </div>
      <div style={{ fontSize: 11, color: '#8c8c8c', minHeight: 16 }}>{sub}</div>
    </Card>
  )
}

/** 使用率进度条配色：高负载转暖色，和告警阈值语义对齐 */
function usageColor(pct: number | null, warn = 85): string {
  if (pct === null) return '#2a3444'
  if (pct >= warn) return '#ff4d4f'
  if (pct >= warn * 0.82) return '#faad14'
  return '#4f9cf9'
}

export default function Dashboard() {
  const [ov, setOv] = useState<OverviewOut | null>(null)
  const [nodes, setNodes] = useState<NodeInfo[]>([])
  const [series, setSeries] = useState<ClusterSeriesOut | null>(null)
  const [range, setRange] = useState(30)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const navigate = useNavigate()

  // ---- 快照数据（汇总 / 告警 / 各节点指标）----
  const loadSnapshot = useCallback(async () => {
    try {
      const [o, n] = await Promise.all([api.overview(), api.listNodes()])
      setOv(o)
      setNodes(n.items)
      setError(null)
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  // ---- 趋势数据（集群资源时间序列）----
  const loadSeries = useCallback(async (minutes: number) => {
    try {
      setSeries(await api.clusterMetrics(minutes))
    } catch {
      /* 趋势图失败不影响主面板 */
    }
  }, [])

  useEffect(() => {
    loadSnapshot()
    const t = window.setInterval(loadSnapshot, 5000)
    return () => window.clearInterval(t)
  }, [loadSnapshot])

  useEffect(() => {
    loadSeries(range)
    const t = window.setInterval(() => loadSeries(range), 10000)
    return () => window.clearInterval(t)
  }, [loadSeries, range])

  // WebSocket 推送的状态变化 → 立即刷新，不等轮询
  useEffect(() => {
    const handler = () => loadSnapshot()
    window.addEventListener(NODE_EVENT, handler)
    return () => window.removeEventListener(NODE_EVENT, handler)
  }, [loadSnapshot])

  const stats = ov?.node_stats
  const res = ov?.resources
  const alerts = ov?.alerts ?? []
  const criticalCount = ov?.alert_summary.critical ?? 0

  // 趋势图数据：CPU/GPU/NPU 三条百分比曲线
  const chartProps = useMemo(() => {
    const items = series?.items ?? []
    return {
      timestamps: items.map((i) => i.ts),
      series: [
        { name: 'CPU 均值', color: METRIC_COLORS.cpu, data: items.map((i) => i.cpu_avg), unit: '%' },
        { name: 'GPU 均值', color: METRIC_COLORS.gpu, data: items.map((i) => i.gpu_avg), unit: '%' },
        { name: 'NPU 均值', color: METRIC_COLORS.npu, data: items.map((i) => i.npu_avg), unit: '%' },
      ],
    }
  }, [series])

  const columns = [
    {
      title: '节点',
      dataIndex: 'name',
      key: 'name',
      render: (_: string, row: NodeInfo) => (
        <Space direction="vertical" size={0}>
          <Space size={6}>
            <a onClick={() => navigate(`/nodes/${row.node_id}`)}>{row.name || row.node_id}</a>
            {row.is_simulated && (
              <Typography.Text type="secondary" style={{ fontSize: 11 }}>
                [模拟]
              </Typography.Text>
            )}
          </Space>
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
      render: (s: NodeInfo['status']) => <NodeStatusTag status={s} />,
    },
    // 指标列：离线/陈旧节点后端给 null，这里一律显示"—"
    ...(
      [
        { title: 'CPU', key: 'cpu', color: METRIC_COLORS.cpu, pct: true },
        { title: '内存', key: 'mem', color: METRIC_COLORS.mem, pct: true },
        { title: '存储', key: 'storage', color: METRIC_COLORS.storage, pct: true },
        { title: '温度', key: 'temp', color: METRIC_COLORS.temp, pct: false },
      ] as const
    ).map((col) => ({
      title: col.title,
      key: col.key,
      width: 100,
      render: (_: unknown, row: NodeInfo) => {
        const m: MetricSample | null = ov?.nodes_metrics?.[row.node_id] ?? null
        if (!m) {
          // 离线 / 采样陈旧：显式"—"，绝不显示最后一次旧值
          return <span style={{ color: '#5a5a5a' }}>—</span>
        }
        let val: number | null = null
        if (col.key === 'cpu') val = m.cpu_usage
        else if (col.key === 'mem') val = ratio(m.mem_used, m.mem_total)
        else if (col.key === 'storage') val = ratio(m.storage_used, m.storage_total)
        else if (col.key === 'temp') val = m.temperature
        if (val === null || val === undefined) {
          return <span style={{ color: '#5a5a5a' }}>—</span>
        }
        const text = col.pct ? `${val.toFixed(1)}%` : `${val.toFixed(1)}℃`
        return (
          <span className="mono" style={{ color: col.color }}>
            {text}
          </span>
        )
      },
    })),
    {
      title: '最后心跳',
      dataIndex: 'last_heartbeat',
      key: 'last_heartbeat',
      width: 200,
      render: (_: string, row: NodeInfo) => (
        <HeartbeatText lastHeartbeat={row.last_heartbeat} status={row.status} />
      ),
    },
    {
      title: '在线时长',
      dataIndex: 'uptime_seconds',
      key: 'uptime_seconds',
      width: 110,
      render: (v: number, row: NodeInfo) =>
        row.status === 'online' ? humanDuration(v) : <span style={{ color: '#8c8c8c' }}>—</span>,
    },
    {
      title: 'IP',
      dataIndex: 'ip',
      key: 'ip',
      width: 140,
      render: (v: string) => <span className="mono">{v || '—'}</span>,
    },
  ]

  if (loading && !ov) {
    return (
      <div style={{ textAlign: 'center', padding: 60 }}>
        <Space direction="vertical">
          <Empty description="正在加载集群状态…" image={Empty.PRESENTED_IMAGE_SIMPLE} />
        </Space>
      </div>
    )
  }

  return (
    <Space direction="vertical" size={16} style={{ width: '100%' }}>
      {error && (
        <Alert
          type="error"
          showIcon
          message="无法连接平台服务端"
          description={`${error}（请确认后端已启动：cd server && python run.py）`}
        />
      )}

      {/* ---------- 汇总卡 ---------- */}
      <Row gutter={[12, 12]}>
        <Col xs={12} sm={8} lg={4}>
          <KpiCard
            title="节点总数"
            value={stats?.total ?? 0}
            sub={`在线 ${stats?.online ?? 0} / 离线 ${stats?.offline ?? 0}`}
            icon={<DashboardOutlined style={{ color: '#4f9cf9' }} />}
          />
        </Col>
        <Col xs={12} sm={8} lg={4}>
          <KpiCard
            title="离线节点"
            value={stats?.offline ?? 0}
            color={stats?.offline ? '#ff4d4f' : '#e6edf3'}
            danger={!!stats?.offline}
            sub={stats?.never_seen ? `另有 ${stats.never_seen} 台未接入` : '全部在线'}
            icon={<CloseCircleOutlined style={{ color: '#ff4d4f' }} />}
          />
        </Col>
        <Col xs={12} sm={8} lg={4}>
          <KpiCard
            title="运行中服务"
            value={ov?.services.running ?? 0}
            sub={`共 ${ov?.services.total ?? 0} 个服务定义`}
            icon={<ApiOutlined style={{ color: '#52c41a' }} />}
          />
        </Col>
        <Col xs={12} sm={8} lg={4}>
          <KpiCard
            title="CPU 平均"
            value={res?.cpu_avg === null || res?.cpu_avg === undefined ? '—' : res.cpu_avg.toFixed(1)}
            suffix={res?.cpu_avg === null ? undefined : '%'}
            sub={`${res?.counted_nodes ?? 0}/${res?.online_nodes ?? 0} 台参与统计`}
            icon={<FireOutlined style={{ color: METRIC_COLORS.cpu }} />}
          />
        </Col>
        <Col xs={12} sm={8} lg={4}>
          <KpiCard
            title="内存池"
            value={res?.mem_percent === null || res?.mem_percent === undefined ? '—' : res.mem_percent.toFixed(1)}
            suffix={res?.mem_percent === null ? undefined : '%'}
            sub={`${humanBytes(res?.mem_used)} / ${humanBytes(res?.mem_total)}`}
            icon={<DatabaseOutlined style={{ color: METRIC_COLORS.mem }} />}
          />
        </Col>
        <Col xs={12} sm={8} lg={4}>
          <KpiCard
            title="存储池"
            value={
              res?.storage_percent === null || res?.storage_percent === undefined
                ? '—'
                : res.storage_percent.toFixed(1)
            }
            suffix={res?.storage_percent === null ? undefined : '%'}
            sub={`${humanBytes(res?.storage_used)} / ${humanBytes(res?.storage_total)}`}
            icon={<DatabaseOutlined style={{ color: METRIC_COLORS.storage }} />}
          />
        </Col>
      </Row>

      {/* 内存/存储使用率条：比纯数字更直观 */}
      <Row gutter={[12, 12]}>
        <Col xs={24} lg={12}>
          <Card size="small" style={{ background: '#1a212b', borderColor: '#2a3444' }}>
            <Space style={{ width: '100%' }} direction="vertical" size={2}>
              <Space style={{ width: '100%', justifyContent: 'space-between' }}>
                <Typography.Text style={{ fontSize: 12, color: '#8c8c8c' }}>集群内存使用率</Typography.Text>
                <span className="mono" style={{ fontSize: 12 }}>
                  {humanPercent(res?.mem_percent)}
                </span>
              </Space>
              <Progress
                percent={res?.mem_percent ?? 0}
                showInfo={false}
                strokeColor={usageColor(res?.mem_percent ?? null)}
                trailColor="#2a3444"
                size="small"
              />
            </Space>
          </Card>
        </Col>
        <Col xs={24} lg={12}>
          <Card size="small" style={{ background: '#1a212b', borderColor: '#2a3444' }}>
            <Space style={{ width: '100%' }} direction="vertical" size={2}>
              <Space style={{ width: '100%', justifyContent: 'space-between' }}>
                <Typography.Text style={{ fontSize: 12, color: '#8c8c8c' }}>集群存储使用率</Typography.Text>
                <span className="mono" style={{ fontSize: 12 }}>
                  {humanPercent(res?.storage_percent)}
                </span>
              </Space>
              <Progress
                percent={res?.storage_percent ?? 0}
                showInfo={false}
                strokeColor={usageColor(res?.storage_percent ?? null, 90)}
                trailColor="#2a3444"
                size="small"
              />
            </Space>
          </Card>
        </Col>
      </Row>

      {/* ---------- 告警 ---------- */}
      <Card
        size="small"
        style={{ background: '#1a212b', borderColor: alerts.length ? '#5c2b2f' : '#2a3444' }}
        title={
          <Space size={8}>
            <span>告警</span>
            {criticalCount > 0 && <Badge count={criticalCount} color="#ff4d4f" />}
          </Space>
        }
        extra={
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            {alerts.length === 0
              ? '当前无告警'
              : `严重 ${ov?.alert_summary.critical ?? 0} · 警告 ${ov?.alert_summary.warning ?? 0}`}
          </Typography.Text>
        }
      >
        {alerts.length === 0 ? (
          <Space size={8}>
            <CheckCircleOutlined style={{ color: '#52c41a' }} />
            <Typography.Text type="secondary" style={{ fontSize: 13 }}>
              所有节点指标正常（阈值见 server/config.toml 的 [alerts] 段）
            </Typography.Text>
          </Space>
        ) : (
          <Space direction="vertical" size={6} style={{ width: '100%' }}>
            {alerts.map((a, i) => {
              const meta = ALERT_KIND_META[a.kind] ?? { text: a.kind, color: '#8c8c8c' }
              return (
                <div
                  key={`${a.node_id}-${a.kind}-${i}`}
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: 10,
                    padding: '4px 8px',
                    borderRadius: 4,
                    background: a.level === 'critical' ? 'rgba(255,77,79,0.08)' : 'rgba(250,173,20,0.07)',
                    borderLeft: `3px solid ${a.level === 'critical' ? '#ff4d4f' : '#faad14'}`,
                  }}
                >
                  <span style={{ color: meta.color, fontSize: 12, minWidth: 68 }}>{meta.text}</span>
                  <a onClick={() => navigate(`/nodes/${a.node_id}`)} style={{ fontSize: 13 }}>
                    {a.node_name || a.node_id}
                  </a>
                  <Typography.Text style={{ fontSize: 13, color: '#d9d9d9' }}>{a.message}</Typography.Text>
                </div>
              )
            })}
          </Space>
        )}
      </Card>

      {/* ---------- 集群资源趋势 ---------- */}
      <Card
        size="small"
        style={{ background: '#1a212b', borderColor: '#2a3444' }}
        title="集群资源趋势（在线节点均值）"
        extra={
          <Space size={8}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              采样粒度 {series?.bucket_seconds ?? 0}s · {series?.count ?? 0} 点
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
        <MetricChart
          timestamps={chartProps.timestamps}
          series={chartProps.series}
          height={260}
          yMax={100}
          valueFormatter={(v) => v.toFixed(0)}
          emptyText="暂无历史采样（节点上线后 5 秒内开始产生）"
        />
        <Typography.Text type="secondary" style={{ fontSize: 11 }}>
          曲线出现断点表示该时刻指标采集不到（手机未暴露该指标），不是数值为 0。
        </Typography.Text>
      </Card>

      {/* ---------- 异常节点 ---------- */}
      {ov && ov.abnormal_nodes.length > 0 && (
        <Card
          size="small"
          style={{ background: '#1a212b', borderColor: '#2a3444' }}
          title={`异常节点（${ov.abnormal_nodes.length}）`}
        >
          <Table
            rowKey="node_id"
            size="small"
            pagination={false}
            dataSource={ov.abnormal_nodes}
            columns={[
              {
                title: '节点',
                key: 'name',
                render: (_: unknown, row) => (
                  <a onClick={() => navigate(`/nodes/${row.node_id}`)}>{row.name || row.node_id}</a>
                ),
              },
              {
                title: '状态',
                dataIndex: 'status',
                key: 'status',
                width: 120,
                render: (s) => <NodeStatusTag status={s} />,
              },
              {
                title: '最后心跳',
                key: 'hb',
                width: 220,
                render: (_: unknown, row) => (
                  <HeartbeatText lastHeartbeat={row.last_heartbeat} status={row.status} />
                ),
              },
              {
                title: 'IP',
                dataIndex: 'ip',
                key: 'ip',
                width: 150,
                render: (v: string) => <span className="mono">{v || '—'}</span>,
              },
              {
                title: '说明',
                key: 'note',
                render: (_: unknown, row) =>
                  row.status === 'offline' ? (
                    <Typography.Text type="danger" style={{ fontSize: 12 }}>
                      已断连，指标停止更新（不显示最后旧值）
                    </Typography.Text>
                  ) : (
                    <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                      从未上报心跳，尚未接入
                    </Typography.Text>
                  ),
              },
            ]}
          />
        </Card>
      )}

      {/* ---------- 节点实时列表 ---------- */}
      <Card
        title="节点实时列表"
        size="small"
        style={{ background: '#1a212b', borderColor: '#2a3444' }}
        extra={
          <Space size={8}>
            {res && res.stale_nodes.length > 0 && (
              <Tooltip title={`这些节点在线，但指标采样已陈旧（>${15}s），已排除在汇总之外`}>
                <Typography.Text type="warning" style={{ fontSize: 12 }}>
                  {res.stale_nodes.length} 台指标陈旧
                </Typography.Text>
              </Tooltip>
            )}
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              状态变化通过 WebSocket 实时推送，5 秒兜底轮询
            </Typography.Text>
          </Space>
        }
      >
        {nodes.length === 0 ? (
          <Empty
            description={
              <span>
                暂无节点。启动一个模拟节点试试：
                <br />
                <code className="mono">
                  cd node-agent &amp;&amp; python agent.py --simulate --name phone-01
                </code>
              </span>
            }
          />
        ) : (
          <Table
            rowKey="node_id"
            size="small"
            columns={columns as never}
            dataSource={nodes}
            pagination={false}
            // 离线行整体压暗，一眼能看出"这台不在线"
            rowClassName={(row) => ((row as NodeInfo).status === 'offline' ? 'row-offline' : '')}
          />
        )}
      </Card>
    </Space>
  )
}
