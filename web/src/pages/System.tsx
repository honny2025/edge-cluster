import { useCallback, useEffect, useState } from 'react'
import {
  Alert,
  Button,
  Card,
  Descriptions,
  Modal,
  Popconfirm,
  Space,
  Statistic,
  Table,
  Tag,
  Typography,
  Upload,
  message,
} from 'antd'
import {
  CloudDownloadOutlined,
  ReloadOutlined,
  SafetyOutlined,
  UploadOutlined,
} from '@ant-design/icons'
import type { UploadFile } from 'antd'
import { api, errText } from '../api'
import { formatDateTime, humanBytes } from '../utils/format'
import type { BackupInspectOut, BackupStatusOut, BackupRestoreOut } from '../types'

const CARD_STYLE = { background: '#1a212b', borderColor: '#2a3444' } as const

/** 恢复流程里"校验过"的那个包。**必须存在它才允许点恢复** —— 没校验过就恢复，
 *  等于把"这个包到底是不是备份"这件事交给运气。 */
type Verified = { file: File; result: BackupInspectOut }

/**
 * 系统页：备份与恢复（A4）。
 *
 * 这个页面是为「换一台电脑」准备的 —— 代码可以直接拷走，但节点记录、模型登记、
 * 部署历史和审计是数据，得靠备份带走。没有这一页，迁移就只能带走一个空壳平台。
 *
 * 两条刻意的设计：
 * 1. **恢复必须先校验**。选好文件先点"校验"，看到绿色的"可以恢复"才能点恢复。
 *    后端也拦：不带 confirm=true 的请求只会返回"将要发生什么"，一个字节都不动。
 * 2. **默认不打包模型权重**。权重动辄几个 GB，多数时候要带走的是"平台的记录"。
 *    需要连权重一起搬时再勾那个开关。
 */
export default function System() {
  const [status, setStatus] = useState<BackupStatusOut | null>(null)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [includeModels, setIncludeModels] = useState(false)

  const [fileList, setFileList] = useState<UploadFile[]>([])
  const [verified, setVerified] = useState<Verified | null>(null)
  const [restoreResult, setRestoreResult] = useState<BackupRestoreOut | null>(null)

  const load = useCallback(async () => {
    setLoading(true)
    try {
      setStatus(await api.backupStatus())
    } catch (e) {
      message.error(errText(e, '读取备份现状失败'))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load()
  }, [load])

  const pick = (f: UploadFile) => {
    setFileList([f])
    setVerified(null) // 换了文件，之前的校验作废
    setRestoreResult(null)
    return false // 不上传给 antd，保留原始 File 自己发
  }

  const doVerify = async () => {
    const raw = fileList[0]?.originFileObj as File | undefined
    if (!raw) {
      message.warning('先选一个备份包')
      return
    }
    setBusy(true)
    try {
      const result = await api.inspectBackup(raw)
      setVerified({ file: raw, result })
      if (result.ok) message.success('校验通过，可以恢复')
      else message.error('校验未通过，不要用它恢复')
    } catch (e) {
      message.error(errText(e, '校验失败'))
    } finally {
      setBusy(false)
    }
  }

  const doRestore = async () => {
    if (!verified) return
    setBusy(true)
    try {
      const res = await api.restoreBackup(verified.file, true)
      setRestoreResult(res)
      message.success('恢复完成，建议重启平台')
      await load()
    } catch (e) {
      message.error(errText(e, '恢复失败'))
    } finally {
      setBusy(false)
    }
  }

  const doBackup = async () => {
    setBusy(true)
    try {
      // 用 Blob 自己造一次下载：直接打开链接会把 X-Auth-Token 丢掉，被 401 拦下
      const { blob, name } = await api.downloadBackup(includeModels)
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = name
      document.body.appendChild(a)
      a.click()
      a.remove()
      URL.revokeObjectURL(url)
      message.success(`已下载 ${name}`)
      await load()
    } catch (e) {
      message.error(errText(e, '备份失败'))
    } finally {
      setBusy(false)
    }
  }

  const v = verified?.result
  const canRestore = Boolean(verified && v?.ok) && !busy

  return (
    <Space direction="vertical" size={16} style={{ width: '100%' }}>
      <Card
        title="备份与恢复"
        size="small"
        style={CARD_STYLE}
        extra={
          <Button icon={<ReloadOutlined />} onClick={load} loading={loading}>
            刷新
          </Button>
        }
      >
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 16 }}
          message="换电脑时，代码直接拷走就行，但节点记录、模型登记、部署历史和审计是数据 —— 靠这里的备份带走。"
          description={
            <span>
              备份走的是 SQLite 的<b>一致性快照</b>（VACUUM INTO），不是复制文件。
              数据库开了 WAL，最近的提交还在 <Typography.Text code>edge.db-wal</Typography.Text> 里没合进主文件，
              直接复制会得到一个「少了最后一截、还不报错」的库。
            </span>
          }
        />
        <Descriptions size="small" column={3} bordered>
          <Descriptions.Item label="数据库">
            {humanBytes(status?.db_size ?? 0)}
          </Descriptions.Item>
          <Descriptions.Item label="未合并的 WAL">
            {status?.wal_present ? (
              <Tag color="orange">有（备份已正确包含）</Tag>
            ) : (
              <Tag color="green">无</Tag>
            )}
          </Descriptions.Item>
          <Descriptions.Item label="已有备份">{status?.backup_count ?? 0} 份</Descriptions.Item>
          <Descriptions.Item label="数据库文件" span={3}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              {status?.db_file}
            </Typography.Text>
          </Descriptions.Item>
          <Descriptions.Item label="备份目录" span={3}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              {status?.backup_dir}
            </Typography.Text>
          </Descriptions.Item>
        </Descriptions>

        <Space style={{ marginTop: 16 }} wrap>
          <Button
            type="primary"
            icon={<CloudDownloadOutlined />}
            loading={busy}
            onClick={doBackup}
          >
            立即备份并下载
          </Button>
          <Button
            icon={<SafetyOutlined />}
            onClick={() => setIncludeModels((x) => !x)}
          >
            {includeModels ? '✓ 包含模型权重' : '不包含模型权重'}
          </Button>
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            默认只带数据库与清单；模型权重动辄几个 GB，需要时再打开左边这个开关。
          </Typography.Text>
        </Space>
      </Card>

      <Card title="从备份恢复" size="small" style={CARD_STYLE}>
        <Alert
          type="warning"
          showIcon
          style={{ marginBottom: 16 }}
          message="恢复会替换当前的全部数据。"
          description="所以流程是两步：先「校验」，确认这个包是好的，才允许点「恢复」。只校验不改数据。真正恢复前平台还会自动把当前数据另存一份（也是走一致性快照，保证那份备份真的能读）。"
        />

        <Space wrap style={{ marginBottom: 16 }}>
          <Upload
            fileList={fileList}
            beforeUpload={pick}
            onRemove={() => {
              setFileList([])
              setVerified(null)
              setRestoreResult(null)
            }}
            maxCount={1}
            accept=".zip"
          >
            <Button icon={<UploadOutlined />}>选择备份包（.zip）</Button>
          </Upload>
          <Button onClick={doVerify} disabled={!fileList.length} loading={busy}>
            校验
          </Button>
          <Popconfirm
            title="确定要恢复吗？"
            description="当前数据会被替换。平台会先自动另存一份当前数据。"
            okText="确定恢复"
            cancelText="取消"
            onConfirm={doRestore}
            disabled={!canRestore}
          >
            <Button danger disabled={!canRestore} loading={busy}>
              恢复
            </Button>
          </Popconfirm>
        </Space>

        {v && (
          <Card size="small" style={{ background: '#14181f', borderColor: '#2a3444' }}>
            <Space direction="vertical" size={8} style={{ width: '100%' }}>
              <Space wrap>
                {v.ok ? (
                  <Tag color="green">可以恢复</Tag>
                ) : (
                  <Tag color="red">不能恢复</Tag>
                )}
                <Tag>备份版本 {String(v.manifest?.app_version ?? '未知')}</Tag>
                <Tag>当前版本 {v.current_app_version}</Tag>
                <Tag>库大小 {humanBytes(v.db_size)}</Tag>
                <Tag color={v.sha256_matches ? 'green' : v.sha256_matches === false ? 'red' : 'default'}>
                  {v.sha256_matches === true
                    ? 'sha256 一致'
                    : v.sha256_matches === false
                      ? 'sha256 不一致'
                      : '未记录 sha256'}
                </Tag>
                <Tag>{v.includes_models ? `含 ${v.model_files_in_package} 个模型文件` : '不含模型权重'}</Tag>
              </Space>
              {v.problems.length > 0 && (
                <Alert
                  type="error"
                  showIcon
                  message="校验未通过的原因"
                  description={
                    <ul style={{ margin: 0, paddingLeft: 18 }}>
                      {v.problems.map((p, i) => (
                        <li key={i}>{p}</li>
                      ))}
                    </ul>
                  }
                />
              )}
            </Space>
          </Card>
        )}

        {restoreResult?.ok && (
          <Alert
            style={{ marginTop: 12 }}
            type="success"
            showIcon
            message="恢复完成"
            description={
              <span>
                {restoreResult.message}
                <br />
                恢复后的行数：
                {Object.entries(restoreResult.tables)
                  .map(([k, n]) => `${k}=${n}`)
                  .join('，')}
                {restoreResult.safety_backup && (
                  <>
                    <br />
                    恢复前的数据已另存为{' '}
                    <Typography.Text code>{restoreResult.safety_backup}</Typography.Text>
                    （放在数据库同目录）。
                  </>
                )}
              </span>
            }
          />
        )}
      </Card>

      <Card title="数据规模" size="small" style={CARD_STYLE}>
        <Space size={32} wrap>
          {Object.entries(status?.tables ?? {}).map(([k, n]) => (
            <Statistic key={k} title={k} value={n} valueStyle={{ fontSize: 20 }} />
          ))}
        </Space>
      </Card>

      <Card title="历史备份" size="small" style={CARD_STYLE}>
        <Table
          size="small"
          rowKey="name"
          loading={loading}
          dataSource={status?.backups ?? []}
          pagination={false}
          locale={{ emptyText: '还没有备份 —— 点上面的「立即备份并下载」' }}
          columns={[
            { title: '文件名', dataIndex: 'name' },
            {
              title: '大小',
              dataIndex: 'size',
              width: 120,
              render: (n: number) => humanBytes(n),
            },
            {
              title: '生成时间',
              dataIndex: 'modified',
              width: 200,
              render: (t: string) => formatDateTime(t),
            },
          ]}
        />
      </Card>
    </Space>
  )
}
