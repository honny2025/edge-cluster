import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Link, Outlet, useLocation } from 'react-router-dom'
import { Badge, Layout, Menu, Space, Tooltip, Typography } from 'antd'
import {
  ApiOutlined,
  CloudServerOutlined,
  DashboardOutlined,
  DatabaseOutlined,
  DeploymentUnitOutlined,
  FileTextOutlined,
  FolderOpenOutlined,
  ReloadOutlined,
  SafetyCertificateOutlined,
  SettingOutlined,
} from '@ant-design/icons'
import { api, dashboardWsUrl } from './api'
import type { SystemInfoOut } from './types'

const { Header, Content, Sider } = Layout

/**
 * 应用外壳：
 * - 顶部展示平台运行信息与在线节点数；
 * - 侧边导航；
 * - 维护一条 Dashboard WebSocket 长连接，收到节点上下线事件时通过
 *   自定义事件广播给子页面，避免各处重复建连。
 */
export const NODE_EVENT = 'edge-cluster:node-event'

export default function App() {
  const location = useLocation()
  const [info, setInfo] = useState<SystemInfoOut | null>(null)
  const [wsState, setWsState] = useState<'connecting' | 'open' | 'closed'>('connecting')
  const wsRef = useRef<WebSocket | null>(null)
  const retryRef = useRef(0)
  const timerRef = useRef<number | null>(null)
  const closedRef = useRef(false)

  const refreshInfo = useCallback(async () => {
    try {
      setInfo(await api.systemInfo())
    } catch {
      /* 平台可能刚重启，静默重试即可 */
    }
  }, [])

  // 平台信息定时刷新（10s）
  useEffect(() => {
    refreshInfo()
    const t = window.setInterval(refreshInfo, 10000)
    return () => window.clearInterval(t)
  }, [refreshInfo])

  // Dashboard 实时事件通道（带指数退避重连）
  useEffect(() => {
    closedRef.current = false

    const connect = () => {
      if (closedRef.current) return
      setWsState('connecting')
      let ws: WebSocket
      try {
        ws = new WebSocket(dashboardWsUrl())
      } catch {
        scheduleRetry()
        return
      }
      wsRef.current = ws

      ws.onopen = () => {
        retryRef.current = 0
        setWsState('open')
      }
      ws.onmessage = (ev) => {
        try {
          const msg = JSON.parse(ev.data)
          if (msg.type === 'event') {
            // 转发给子页面
            window.dispatchEvent(new CustomEvent(NODE_EVENT, { detail: msg }))
            refreshInfo()
          }
        } catch {
          /* 忽略非法消息 */
        }
      }
      ws.onclose = () => {
        setWsState('closed')
        scheduleRetry()
      }
      ws.onerror = () => {
        try {
          ws.close()
        } catch {
          /* ignore */
        }
      }
    }

    const scheduleRetry = () => {
      if (closedRef.current) return
      if (timerRef.current) window.clearTimeout(timerRef.current)
      const delay = Math.min(30000, 1000 * 2 ** retryRef.current)
      retryRef.current += 1
      timerRef.current = window.setTimeout(connect, delay)
    }

    connect()
    return () => {
      closedRef.current = true
      if (timerRef.current) window.clearTimeout(timerRef.current)
      try {
        wsRef.current?.close()
      } catch {
        /* ignore */
      }
    }
  }, [refreshInfo])

  const selectedKey = useMemo(() => {
    if (location.pathname.startsWith('/nodes')) return '/nodes'
    if (location.pathname.startsWith('/models')) return '/models'
    if (location.pathname.startsWith('/deployments')) return '/deployments'
    if (location.pathname.startsWith('/services')) return '/services'
    if (location.pathname.startsWith('/files')) return '/files'
    if (location.pathname.startsWith('/audit')) return '/audit'
    return '/'
  }, [location.pathname])

  const stats = info?.node_stats

  return (
    <Layout style={{ minHeight: '100vh' }}>
      <Header
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
          padding: '0 20px',
          background: 'linear-gradient(180deg, rgba(28, 36, 48, 0.94), rgba(20, 26, 36, 0.94))',
          borderBottom: '1px solid rgba(56, 189, 248, 0.20)',
          boxShadow: '0 4px 24px rgba(0, 0, 0, 0.35)',
        }}
      >
        <Space size={12}>
          <CloudServerOutlined style={{ fontSize: 20, color: '#38bdf8', filter: 'drop-shadow(0 0 6px rgba(56,189,248,0.6))' }} />
          <Typography.Text strong style={{ fontSize: 16 }}>
            手机边缘计算集群管理平台
          </Typography.Text>
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            v{info?.version ?? '—'}
          </Typography.Text>
        </Space>
        <Space size={20}>
          <Tooltip title="平台与浏览器的实时通道状态">
            <Space size={6}>
              <span
                className={`status-dot ${
                  wsState === 'open' ? 'online' : wsState === 'closed' ? 'offline' : 'never_seen'
                }`}
              />
              <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                实时通道{wsState === 'open' ? '已连接' : wsState === 'closed' ? '重连中' : '连接中'}
              </Typography.Text>
            </Space>
          </Tooltip>
          <Space size={16}>
            <span>
              <Badge status="success" />
              <Typography.Text style={{ fontSize: 13 }}>在线 {stats?.online ?? 0}</Typography.Text>
            </span>
            <span>
              <Badge status="error" />
              <Typography.Text style={{ fontSize: 13 }}>离线 {stats?.offline ?? 0}</Typography.Text>
            </span>
            <span>
              <Badge status="default" />
              <Typography.Text style={{ fontSize: 13 }}>
                从未上线 {stats?.never_seen ?? 0}
              </Typography.Text>
            </span>
          </Space>
          <Tooltip title="刷新平台信息">
            <ReloadOutlined onClick={refreshInfo} style={{ cursor: 'pointer' }} />
          </Tooltip>
        </Space>
      </Header>

      <Layout>
        <Sider
          width={200}
          style={{
            background: 'linear-gradient(180deg, rgba(24, 32, 44, 0.96), rgba(15, 20, 30, 0.96))',
            borderRight: '1px solid rgba(56, 189, 248, 0.12)',
          }}
        >
          <Menu
            mode="inline"
            selectedKeys={[selectedKey]}
            style={{ background: 'transparent', borderInlineEnd: 'none' }}
            items={[
              {
                key: '/',
                icon: <DashboardOutlined />,
                label: <Link to="/">总览</Link>,
              },
              {
                key: '/nodes',
                icon: <CloudServerOutlined />,
                label: <Link to="/nodes">节点</Link>,
              },
              {
                key: '/models',
                icon: <DatabaseOutlined />,
                label: <Link to="/models">模型库</Link>,
              },
              {
                key: '/deployments',
                icon: <DeploymentUnitOutlined />,
                label: <Link to="/deployments">部署</Link>,
              },
              {
                key: '/services',
                icon: <ApiOutlined />,
                label: <Link to="/services">服务</Link>,
              },
              {
                key: '/files',
                icon: <FolderOpenOutlined />,
                label: <Link to="/files">文件</Link>,
              },
              {
                key: '/audit',
                icon: <FileTextOutlined />,
                label: <Link to="/audit">审计</Link>,
              },
              {
                key: '/system',
                icon: <SettingOutlined />,
                label: <Link to="/system">系统</Link>,
              },
              {
                key: '/docs',
                icon: <SafetyCertificateOutlined />,
                label: (
                  <a href="/docs" target="_blank" rel="noreferrer">
                    接口文档
                  </a>
                ),
              },
            ]}
          />
        </Sider>
        <Content style={{ padding: 20, background: 'transparent' }}>
          <Outlet context={{ info, refreshInfo }} />
        </Content>
      </Layout>
    </Layout>
  )
}
