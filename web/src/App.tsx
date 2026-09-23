import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Link, Outlet, useLocation } from 'react-router-dom'
import { Badge, Layout, Menu, Space, Tooltip, Typography } from 'antd'
import {
  ApiOutlined,
  CloudServerOutlined,
  DashboardOutlined,
  ReloadOutlined,
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
          background: '#1c2430',
          borderBottom: '1px solid #2a3444',
        }}
      >
        <Space size={12}>
          <CloudServerOutlined style={{ fontSize: 20, color: '#4f9cf9' }} />
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
        <Sider width={200} style={{ background: '#1a212b' }}>
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
                icon: <ApiOutlined />,
                label: <Link to="/nodes">节点</Link>,
              },
            ]}
          />
        </Sider>
        <Content style={{ padding: 20, background: '#14181f' }}>
          <Outlet context={{ info, refreshInfo }} />
        </Content>
      </Layout>
    </Layout>
  )
}
