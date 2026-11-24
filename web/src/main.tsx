import React from 'react'
import ReactDOM from 'react-dom/client'
import { BrowserRouter, Navigate, Route, Routes } from 'react-router-dom'
import { ConfigProvider, theme as antdTheme } from 'antd'
import zhCN from 'antd/locale/zh_CN'
import App from './App'
import Dashboard from './pages/Dashboard'
import Nodes from './pages/Nodes'
import NodeDetail from './pages/NodeDetail'
import Models from './pages/Models'
import Deployments from './pages/Deployments'
import Services from './pages/Services'
import Files from './pages/Files'
import Audit from './pages/Audit'
import System from './pages/System'
import './index.css'

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ConfigProvider
      locale={zhCN}
      theme={{
        algorithm: antdTheme.darkAlgorithm,
        token: {
          colorPrimary: '#38bdf8',
          colorInfo: '#38bdf8',
          borderRadius: 10,
          fontSize: 14,
          colorBgContainer: '#1a2230',
          colorBgElevated: '#1c2533',
          colorBorder: 'rgba(56, 189, 248, 0.18)',
          colorBorderSecondary: 'rgba(56, 189, 248, 0.10)',
          colorText: '#e6edf3',
          colorTextSecondary: '#9aa8ba',
        },
        components: {
          Layout: {
            headerBg: 'transparent',
            siderBg: 'transparent',
            bodyBg: 'transparent',
          },
          Menu: {
            darkItemBg: 'transparent',
            darkItemSelectedBg: 'rgba(56, 189, 248, 0.16)',
            darkItemSelectedColor: '#38bdf8',
            itemBorderRadius: 8,
          },
          Table: {
            headerBg: 'rgba(56, 189, 248, 0.07)',
            headerColor: '#cfe6f5',
            headerSplitColor: 'rgba(56, 189, 248, 0.18)',
            rowHoverBg: 'rgba(56, 189, 248, 0.06)',
          },
        },
      }}
    >
      <BrowserRouter>
        <Routes>
          <Route element={<App />}>
          <Route path="/" element={<Dashboard />} />
          <Route path="/nodes" element={<Nodes />} />
          <Route path="/nodes/:nodeId" element={<NodeDetail />} />
          <Route path="/models" element={<Models />} />
          <Route path="/deployments" element={<Deployments />} />
          <Route path="/services" element={<Services />} />
          <Route path="/files" element={<Files />} />
          <Route path="/audit" element={<Audit />} />
          <Route path="/system" element={<System />} />
          <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
      </BrowserRouter>
    </ConfigProvider>
  </React.StrictMode>,
)
