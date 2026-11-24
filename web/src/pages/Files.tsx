import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import {
  Alert,
  Breadcrumb,
  Button,
  Card,
  Empty,
  Input,
  Modal,
  Popconfirm,
  Progress,
  Select,
  Space,
  Table,
  Tag,
  Tooltip,
  Typography,
  message,
} from 'antd'
import {
  DownloadOutlined,
  FolderOutlined,
  HomeOutlined,
  ReloadOutlined,
  UploadOutlined,
} from '@ant-design/icons'
import { api, errText, transferDownloadUrl } from '../api'
import { formatDateTime, humanBytes } from '../utils/format'
import type { FsEntry, FsRoot, NodeInfo, TransferOut } from '../types'
import { TRANSFER_DIRECTION_META, TRANSFER_STATUS_META } from '../types'

const CARD_STYLE = { background: '#1a212b', borderColor: '#2a3444' } as const

/** 后端把"路径不存在"翻译成 404；这里据此区分"东西没了"和"路径有问题" */
function isNotFound(e: unknown): boolean {
  return (e as { response?: { status?: number } })?.response?.status === 404
}

export default function Files() {
  const [params, setParams] = useSearchParams()
  const nodeId = params.get('node') ?? ''
  const [nodes, setNodes] = useState<NodeInfo[]>([])
  const [roots, setRoots] = useState<FsRoot[]>([])
  const [path, setPath] = useState('')
  const [entries, setEntries] = useState<FsEntry[]>([])
  const [listing, setListing] = useState(false)
  const [space, setSpace] = useState<{ total: number | null; free: number | null } | null>(null)
  const [truncated, setTruncated] = useState(false)
  const [total, setTotal] = useState(0)
  const [err, setErr] = useState('')
  const [transfers, setTransfers] = useState<TransferOut[]>([])
  const [busy, setBusy] = useState('')
  const fileInputRef = useRef<HTMLInputElement>(null)

  // ---- 新建目录 ----
  const [mkdirOpen, setMkdirOpen] = useState(false)
  const [mkdirName, setMkdirName] = useState('')
  // ---- 重命名/移动 ----
  const [moveOpen, setMoveOpen] = useState(false)
  const [moveSrc, setMoveSrc] = useState<FsEntry | null>(null)
  const [moveDst, setMoveDst] = useState('')

  const onlineNodes = useMemo(() => nodes.filter((n) => n.status === 'online'), [nodes])

  useEffect(() => {
    api
      .listNodes()
      .then((d) => {
        setNodes(d.items)
        // 没指定节点就默认选第一台在线的：让用户一进来就有东西可看
        if (!nodeId) {
          const first = d.items.find((n) => n.status === 'online')
          if (first) setParams({ node: first.node_id }, { replace: true })
        }
      })
      .catch((e) => message.error(errText(e, '获取节点列表失败')))
    // nodeId 只用于"默认选中"，不作为依赖 —— 否则选中后会把用户改过的选择重置
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [setParams])

  /** 拉取根清单。根由 Agent 自报，平台不写死任何路径 */
  const loadRoots = useCallback(async (nid: string) => {
    try {
      const d = await api.fsRoots(nid)
      setRoots(d.roots ?? [])
      return d.roots ?? []
    } catch (e) {
      setRoots([])
      message.error(errText(e, '获取设备可访问目录失败'))
      return []
    }
  }, [])

  const browse = useCallback(
    async (nid: string, p: string, quiet = false) => {
      if (!nid) return
      setListing(true)
      setErr('')
      try {
        const d = await api.fsList(nid, p)
        setEntries(d.entries ?? [])
        setPath(d.path)
        setSpace(d.space ?? null)
        setTruncated(Boolean(d.truncated))
        setTotal(d.total ?? 0)
      } catch (e) {
        setEntries([])
        if (isNotFound(e)) {
          setErr('这个路径不存在了（可能刚被删除或改名）')
        } else {
          setErr(errText(e, '打开目录失败'))
        }
        if (!quiet) message.error(errText(e, '打开目录失败'))
      } finally {
        setListing(false)
      }
    },
    [],
  )

  const loadTransfers = useCallback(async (nid: string) => {
    if (!nid) return
    try {
      const d = await api.listTransfers(nid, 50)
      setTransfers(d.items ?? [])
    } catch {
      /* 传输记录是辅助信息，拉不到不影响浏览 */
    }
  }, [])

  useEffect(() => {
    if (!nodeId) {
      setRoots([])
      setEntries([])
      setPath('')
      return
    }
    loadRoots(nodeId).then((rs) => {
      // 直接列出第一个根的顶层，省掉用户一次点击
      browse(nodeId, rs.length ? rs[0].path : '')
    })
    loadTransfers(nodeId)
  }, [nodeId, loadRoots, browse, loadTransfers])

  // 有未完成传输时轮询 —— 传完之后就停下，不做无谓请求
  useEffect(() => {
    const pending = transfers.some((t) => t.status === 'pending' || t.status === 'transferring')
    if (!pending || !nodeId) return
    const t = window.setInterval(() => loadTransfers(nodeId), 2000)
    return () => window.clearInterval(t)
  }, [transfers, nodeId, loadTransfers])

  // ---------------------------------------------------------------- 操作

  const openEntry = (e: FsEntry) => {
    if (e.is_dir) browse(nodeId, e.path)
  }

  const goParent = () => {
    if (!path) return
    const norm = path.replace(/\\/g, '/').replace(/\/+$/, '')
    const idx = norm.lastIndexOf('/')
    if (idx <= 0) return
    browse(nodeId, norm.slice(0, idx))
  }

  const doMkdir = async () => {
    const name = mkdirName.trim()
    if (!name) {
      message.warning('请输入目录名')
      return
    }
    setBusy('mkdir')
    try {
      const sep = path.includes('\\') ? '\\' : '/'
      await api.fsMkdir(nodeId, `${path.replace(/[\\/]+$/, '')}${sep}${name}`)
      message.success('目录已创建')
      setMkdirOpen(false)
      setMkdirName('')
      browse(nodeId, path, true)
    } catch (e) {
      message.error(errText(e, '创建目录失败'))
    } finally {
      setBusy('')
    }
  }

  const doDelete = async (e: FsEntry) => {
    setBusy(e.path)
    try {
      // 目录必须显式 recursive —— 后端默认拒绝，防的是"手滑删掉整棵树"。
      // 界面上已经用红色的确认文案把这个后果说清楚了。
      await api.fsDelete(nodeId, e.path, e.is_dir)
      message.success('已删除')
      browse(nodeId, path, true)
    } catch (err2) {
      message.error(errText(err2, '删除失败'))
    } finally {
      setBusy('')
    }
  }

  const doPull = async (e: FsEntry) => {
    setBusy(e.path)
    try {
      await api.fsPull(nodeId, e.path)
      message.success('已通知设备上传，可在下方「传输记录」中取回')
      loadTransfers(nodeId)
    } catch (err2) {
      message.error(errText(err2, '取回文件失败'))
    } finally {
      setBusy('')
    }
  }

  const doPush = async (file: File) => {
    setBusy('push')
    try {
      const sep = path.includes('\\') ? '\\' : '/'
      const dest = `${path.replace(/[\\/]+$/, '')}${sep}${file.name}`
      await api.fsPush(nodeId, dest, file)
      message.success(`已把 ${file.name} 推送到设备`)
      loadTransfers(nodeId)
      browse(nodeId, path, true)
    } catch (e) {
      message.error(errText(e, '上传失败'))
    } finally {
      setBusy('')
    }
  }

  const doMove = async () => {
    if (!moveSrc) return
    const dst = moveDst.trim()
    if (!dst) {
      message.warning('请输入目标路径')
      return
    }
    setBusy(moveSrc.path)
    try {
      await api.fsMove(nodeId, moveSrc.path, dst)
      message.success('已移动/重命名')
      setMoveOpen(false)
      setMoveSrc(null)
      browse(nodeId, path, true)
    } catch (e) {
      message.error(errText(e, '移动失败'))
    } finally {
      setBusy('')
    }
  }

  const cancelTransfer = async (t: TransferOut, doneLabel = '已取消传输') => {
    try {
      await api.cancelTransfer(t.transfer_id)
      message.success(doneLabel)
      loadTransfers(nodeId)
    } catch (e) {
      message.error(errText(e, '操作失败'))
    }
  }

  // ---------------------------------------------------------------- 渲染

  const rootOptions = roots.map((r) => ({
    value: r.path,
    label: `${r.label}${r.writable ? '' : '（只读）'} · 剩余 ${humanBytes(r.free)}`,
  }))

  const currentRoot = roots.find((r) => path === r.path || path.startsWith(r.path))
  const writable = currentRoot ? currentRoot.writable : false

  const columns = [
    {
      title: '名称',
      dataIndex: 'name',
      key: 'name',
      render: (_: string, r: FsEntry) => (
        <Space size={6}>
          {r.is_dir ? <FolderOutlined style={{ color: '#4f9cf9' }} /> : null}
          {r.is_dir ? (
            <a onClick={() => openEntry(r)}>{r.name}</a>
          ) : (
            <span>{r.name}</span>
          )}
          {r.is_link && <Tag style={{ marginInlineEnd: 0 }}>链接</Tag>}
        </Space>
      ),
    },
    {
      title: '大小',
      dataIndex: 'size',
      key: 'size',
      width: 110,
      // 读不到属性的条目 size 为 null —— 显示「—」而不是 0
      render: (v: number | null, r: FsEntry) => (r.is_dir ? '目录' : humanBytes(v)),
    },
    {
      title: '修改时间',
      dataIndex: 'mtime',
      key: 'mtime',
      width: 175,
      render: (v: number | null) =>
        v ? formatDateTime(new Date(v * 1000).toISOString().replace('Z', '')) : '—',
    },
    {
      title: '操作',
      key: 'action',
      width: 230,
      render: (_: unknown, r: FsEntry) => (
        <Space size={10} wrap>
          {r.is_dir ? (
            <a onClick={() => openEntry(r)}>打开</a>
          ) : (
            <Tooltip title="让设备把它上传到平台，随后可在下方传输记录里下载">
              <a onClick={() => doPull(r)}>取回</a>
            </Tooltip>
          )}
          <a
            onClick={() => {
              setMoveSrc(r)
              setMoveDst(r.path)
              setMoveOpen(true)
            }}
          >
            重命名
          </a>
          <Popconfirm
            title={r.is_dir ? `删除目录「${r.name}」及其全部内容？` : `删除「${r.name}」？`}
            description={r.is_dir ? '目录会连同里面的所有文件一起删除，无法撤销。' : '删除后无法撤销。'}
            okText="删除"
            okButtonProps={{ danger: true }}
            cancelText="取消"
            onConfirm={() => doDelete(r)}
          >
            <a style={{ color: '#ff7875' }}>删除</a>
          </Popconfirm>
        </Space>
      ),
    },
  ]

  const transferColumns = [
    {
      title: '方向',
      dataIndex: 'direction',
      key: 'dir',
      width: 110,
      render: (v: keyof typeof TRANSFER_DIRECTION_META) =>
        TRANSFER_DIRECTION_META[v]?.text ?? v,
    },
    {
      title: '文件',
      dataIndex: 'file_name',
      key: 'name',
      render: (v: string, r: TransferOut) => (
        <Space direction="vertical" size={0}>
          <span>{v}</span>
          <Typography.Text type="secondary" className="mono" style={{ fontSize: 11 }}>
            {r.path}
          </Typography.Text>
        </Space>
      ),
    },
    {
      title: '状态',
      dataIndex: 'status',
      key: 'status',
      width: 100,
      render: (v: keyof typeof TRANSFER_STATUS_META) => {
        const m = TRANSFER_STATUS_META[v] ?? { text: v, color: 'default' }
        return <Tag color={m.color}>{m.text}</Tag>
      },
    },
    {
      title: '进度',
      key: 'progress',
      width: 180,
      render: (_: unknown, r: TransferOut) => {
        const total = r.size || 0
        const done = r.bytes_done || 0
        const finished = ['done', 'failed', 'cancelled', 'expired'].includes(r.status)
        const transferring = r.status === 'pending' || r.status === 'transferring'
        const percent = finished
          ? 100
          : total > 0
            ? Math.min(99.9, (done / total) * 100)
            : 0
        return (
          <Space direction="vertical" size={2} style={{ width: '100%' }}>
            <Progress
              percent={percent}
              size="small"
              status={r.status === 'failed' ? 'exception' : r.status === 'done' ? 'success' : 'active'}
              showInfo={false}
            />
            <Typography.Text type="secondary" style={{ fontSize: 11 }}>
              {transferring && total > 0
                ? `${humanBytes(done)} / ${humanBytes(total)} (${percent.toFixed(0)}%)`
                : humanBytes(total)}
              {r.error ? ` · ${r.error}` : ''}
            </Typography.Text>
          </Space>
        )
      },
    },
    {
      title: '操作',
      key: 'action',
      width: 140,
      render: (_: unknown, r: TransferOut) => {
        const active = r.status === 'pending' || r.status === 'transferring'
        // 传输中只有「取消」；进入终态后给「下载」（仅 pull+done）+「删除」两个入口。
        if (active) {
          return <a onClick={() => cancelTransfer(r)}>取消</a>
        }
        // 只有 done 才给下载：半截文件比没有文件更危险 —— 用户会以为拿到了完整数据
        const download =
          r.direction === 'pull' && r.status === 'done' ? (
            // 不带 target="_blank"：桌面壳 WebView2 里 _blank 会触发「开新窗口」被拦下、
            // 走不到下载；不带 target 则在当前窗口导航，server 的 Content-Disposition
            // attachment 会触发 on_download 弹保存框，普通浏览器里同样能正常下载。
            <a href={transferDownloadUrl(r.transfer_id, r.file_name)}>
              <DownloadOutlined /> 下载
            </a>
          ) : null
        // done 也要能删（把暂存文件清掉）；failed/cancelled/expired 同样给清理入口。
        return (
          <Space size={12}>
            {download}
            <Popconfirm
              title="删除这条传输记录？"
              description="会同时清理平台暂存文件。"
              onConfirm={() => cancelTransfer(r, '已删除')}
              okText="删除"
              cancelText="返回"
            >
              <a style={{ color: '#ff4d4f' }}>删除</a>
            </Popconfirm>
          </Space>
        )
      },
    },
  ]

  return (
    <Space direction="vertical" size={16} style={{ width: '100%' }}>
      <Card
        title="文件管理"
        size="small"
        style={CARD_STYLE}
        extra={
          <Space>
            <Select
              style={{ minWidth: 260 }}
              placeholder="选择节点"
              value={nodeId || undefined}
              onChange={(v) => setParams({ node: v })}
              options={onlineNodes.map((n) => ({ value: n.node_id, label: n.name || n.node_id }))}
              notFoundContent="没有在线节点"
            />
            <Button icon={<ReloadOutlined />} onClick={() => nodeId && browse(nodeId, path)}>
              刷新
            </Button>
          </Space>
        }
      >
        {!nodeId ? (
          <Empty description="请选择一台在线节点" />
        ) : (
          <>
            <Space style={{ marginBottom: 10 }} wrap>
              <Select
                style={{ minWidth: 300 }}
                placeholder="设备上的可访问目录"
                value={roots.some((r) => r.path === path) ? path : undefined}
                onChange={(v) => browse(nodeId, v)}
                options={rootOptions}
                notFoundContent="设备未上报任何可访问目录"
              />
              <Input
                style={{ width: 420 }}
                value={path}
                onChange={(e) => setPath(e.target.value)}
                onPressEnter={() => browse(nodeId, path)}
                placeholder="路径"
                className="mono"
              />
              <Button onClick={() => browse(nodeId, path)} loading={listing}>
                转到
              </Button>
              <Button icon={<HomeOutlined />} disabled={!path} onClick={goParent}>
                上一级
              </Button>
            </Space>

            <Space style={{ marginBottom: 10 }} wrap>
              <Button
                icon={<FolderOutlined />}
                disabled={!writable}
                onClick={() => {
                  setMkdirName('')
                  setMkdirOpen(true)
                }}
              >
                新建目录
              </Button>
              <input
                ref={fileInputRef}
                type="file"
                style={{ display: 'none' }}
                disabled={!writable}
                onChange={(e) => {
                  const f = e.target.files?.[0]
                  if (f) doPush(f)
                  e.target.value = ''
                }}
              />
              <Button
                icon={<UploadOutlined />}
                disabled={!writable}
                loading={busy === 'push'}
                onClick={() => fileInputRef.current?.click()}
              >
                上传到该目录
              </Button>
              {!writable && (
                <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                  当前目录只读，无法写入
                </Typography.Text>
              )}
              <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                {total} 项{truncated ? `（仅显示前 ${entries.length} 项）` : ''}
              </Typography.Text>
            </Space>

            <Breadcrumb
              style={{ marginBottom: 10, fontSize: 12 }}
              items={[
                ...(currentRoot
                  ? [
                      {
                        title: <a onClick={() => browse(nodeId, currentRoot.path)}>{currentRoot.label}</a>,
                      },
                    ]
                  : []),
                {
                  title: (
                    <span className="mono" style={{ fontSize: 11 }}>
                      {path}
                    </span>
                  ),
                },
              ]}
            />

            {err && (
              <Alert
                type={err.includes('不存在') ? 'warning' : 'error'}
                showIcon
                style={{ marginBottom: 10 }}
                message={err}
              />
            )}

            {space && (
              <Alert
                type="info"
                showIcon
                style={{ marginBottom: 10 }}
                message={
                  space.total && space.free !== null
                    ? `该分区剩余 ${humanBytes(space.free)} / 共 ${humanBytes(space.total)}`
                    : '该分区剩余空间：设备未提供'
                }
              />
            )}

            <Table
              rowKey="path"
              size="small"
              loading={listing}
              columns={columns as never}
              dataSource={entries}
              pagination={{ pageSize: 20, showSizeChanger: false }}
              locale={{ emptyText: <Empty description="空目录" image={Empty.PRESENTED_IMAGE_SIMPLE} /> }}
            />

            <Alert
              type="warning"
              showIcon
              style={{ marginTop: 10, fontSize: 12 }}
              message="文件操作的作用范围由设备上的 Agent 沙箱决定，平台不写死任何路径，也无法越出沙箱。"
            />
          </>
        )}
      </Card>

      <Card
        title="传输记录"
        size="small"
        style={CARD_STYLE}
        extra={
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            取回的文件保存在平台暂存区，下载后请及时取走
          </Typography.Text>
        }
      >
        <Table
          rowKey="transfer_id"
          size="small"
          columns={transferColumns as never}
          dataSource={transfers}
          pagination={{ pageSize: 10, showSizeChanger: false }}
          locale={{ emptyText: <Empty description="暂无传输记录" image={Empty.PRESENTED_IMAGE_SIMPLE} /> }}
        />
      </Card>

      <Modal
        title="新建目录"
        open={mkdirOpen}
        onCancel={() => setMkdirOpen(false)}
        onOk={doMkdir}
        confirmLoading={busy === 'mkdir'}
        okText="创建"
        cancelText="取消"
      >
        <div style={{ marginBottom: 4 }}>
          在 <span className="mono">{path}</span> 下创建：
        </div>
        <Input
          value={mkdirName}
          onChange={(e) => setMkdirName(e.target.value)}
          onPressEnter={doMkdir}
          placeholder="目录名"
          autoFocus
        />
      </Modal>

      <Modal
        title="重命名 / 移动"
        open={moveOpen}
        onCancel={() => setMoveOpen(false)}
        onOk={doMove}
        confirmLoading={busy === moveSrc?.path}
        okText="确定"
        cancelText="取消"
      >
        <div style={{ marginBottom: 4 }}>从：</div>
        <Input value={moveSrc?.path ?? ''} disabled className="mono" />
        <div style={{ margin: '8px 0 4px' }}>到（完整路径）：</div>
        <Input
          value={moveDst}
          onChange={(e) => setMoveDst(e.target.value)}
          onPressEnter={doMove}
          className="mono"
          autoFocus
        />
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          改同目录下的文件名 = 重命名；改目录部分 = 移动。
        </Typography.Text>
      </Modal>
    </Space>
  )
}
