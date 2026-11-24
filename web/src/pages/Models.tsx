import { useCallback, useEffect, useState } from 'react'
import {
  Alert,
  Button,
  Card,
  Descriptions,
  Divider,
  Drawer,
  Empty,
  Form,
  Input,
  InputNumber,
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
import { ReloadOutlined, UploadOutlined } from '@ant-design/icons'
import { api, errText, manifestDownloadUrl, modelDownloadUrl } from '../api'
import { formatDateTime, humanBytes } from '../utils/format'
import type {
  CompatCheck,
  CompatVerdict,
  Framework,
  ManifestItem,
  ModelOut,
  NodeInfo,
  PrecheckNodeResult,
} from '../types'
import { MANIFEST_KIND_META } from '../types'

const CARD_STYLE = { background: '#1a212b', borderColor: '#2a3444' } as const

const FRAMEWORKS: { value: Framework; label: string }[] = [
  { value: 'llama.cpp', label: 'llama.cpp' },
  { value: 'onnx', label: 'ONNX' },
  { value: 'mnn', label: 'MNN' },
  { value: 'qnn', label: 'QNN' },
  { value: 'other', label: '其他' },
]

const MANIFEST_KINDS = Object.entries(MANIFEST_KIND_META).map(([value, label]) => ({
  value,
  label,
}))

/** 兼容性判定 → 标签。unknown 必须和 fail 区分开：那是"不知道"，不是"不行" */
const VERDICT_META: Record<CompatVerdict, { text: string; color: string }> = {
  pass: { text: '通过', color: 'success' },
  fail: { text: '不兼容', color: 'error' },
  unknown: { text: '未登记', color: 'default' },
}

/** 空值统一显示成"—"：**空串不是"没有"，是"没登记"** */
function dash(v: unknown): string {
  const s = v === null || v === undefined ? '' : String(v).trim()
  return s || '—'
}

/** 把用户填的 JSON 文本解析成数组/对象；空串返回 undefined（表示"不改这一项"） */
function parseJsonField(raw: string, kind: 'list' | 'dict'): unknown {
  const s = (raw || '').trim()
  if (!s) return undefined
  const parsed = JSON.parse(s)
  if (kind === 'list' && !Array.isArray(parsed)) {
    throw new Error('需要 JSON 数组，例如 ["htp","cpu"]')
  }
  if (kind === 'dict' && (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed))) {
    throw new Error('需要 JSON 对象，例如 {"n_ctx":2048}')
  }
  return parsed
}

export default function Models() {
  const [models, setModels] = useState<ModelOut[]>([])
  const [nodes, setNodes] = useState<NodeInfo[]>([])
  const [loading, setLoading] = useState(true)
  const [open, setOpen] = useState(false)
  const [uploading, setUploading] = useState(false)
  const [uploadProgress, setUploadProgress] = useState<number | null>(null)
  const [detail, setDetail] = useState<ModelOut | null>(null)
  const [form] = Form.useForm()
  const [file, setFile] = useState<File | null>(null)

  const load = useCallback(async (showSpin = false) => {
    if (showSpin) setLoading(true)
    try {
      const data = await api.listModels()
      setModels(data.items)
    } catch (e) {
      message.error(errText(e, '加载模型库失败'))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load(true)
    api
      .listNodes()
      .then((d) => setNodes(d.items))
      .catch(() => {
        /* 兼容性预检用；拿不到节点就让用户看到"暂无节点"而不是报错 */
      })
  }, [load])

  // ------------------------------------------------------------------ 上传

  const submit = async () => {
    let values: Record<string, unknown>
    try {
      values = await form.validateFields()
    } catch {
      return
    }
    if (!file) {
      message.warning('请选择模型文件')
      return
    }
    let backends: unknown
    let deps: unknown
    let runParams: unknown
    try {
      backends = parseJsonField(String(values.backends ?? ''), 'list')
      deps = parseJsonField(String(values.deps ?? ''), 'list')
      runParams = parseJsonField(String(values.run_params ?? ''), 'dict')
    } catch (e) {
      message.error(`JSON 字段格式不对：${(e as Error).message}`)
      return
    }

    setUploading(true)
    setUploadProgress(0)
    try {
      const fd = new FormData()
      fd.append('file', file)
      // 只 append 有值的字段 —— 空串会被后端存成空串，
      // 和"没登记"是同一回事，但少传几个字段能让请求体更干净
      const put = (k: string, v: unknown) => {
        if (v === undefined || v === null || v === '') return
        fd.append(k, typeof v === 'object' ? JSON.stringify(v) : String(v))
      }
      put('name', values.name)
      put('version', values.version || 'v1')
      put('framework', values.framework || 'other')
      put('format', values.format)
      put('description', values.description)
      put('quantization', values.quantization)
      put('target_soc', values.target_soc)
      put('qnn_version', values.qnn_version)
      put('self_test_command', values.self_test_command)
      put('self_test_timeout', values.self_test_timeout)
      put('backends', backends)
      put('deps', deps)
      put('run_params', runParams)

      await api.uploadModel(fd, setUploadProgress)
      message.success('模型已入库')
      setOpen(false)
      form.resetFields()
      setFile(null)
      load(false)
    } catch (e) {
      message.error(errText(e, '上传失败'))
    } finally {
      setUploading(false)
      setUploadProgress(null)
    }
  }

  const remove = async (id: string) => {
    try {
      await api.deleteModel(id)
      message.success('已删除模型（含文件与附属物）')
      if (detail?.model_id === id) setDetail(null)
      load(false)
    } catch (e) {
      message.error(errText(e, '删除失败'))
    }
  }

  const columns = [
    {
      title: '名称',
      dataIndex: 'name',
      key: 'name',
      render: (_: string, r: ModelOut) => (
        <Space direction="vertical" size={0}>
          <a onClick={() => setDetail(r)}>{r.name}</a>
          <Typography.Text type="secondary" className="mono" style={{ fontSize: 11 }}>
            {r.version}
          </Typography.Text>
        </Space>
      ),
    },
    { title: '框架', dataIndex: 'framework', key: 'framework', width: 100, render: dash },
    {
      title: '量化',
      dataIndex: 'quantization',
      key: 'quant',
      width: 100,
      render: dash,
    },
    {
      title: '目标 SoC',
      dataIndex: 'target_soc',
      key: 'soc',
      width: 130,
      render: (v: string) => (
        <Typography.Text className="mono" style={{ fontSize: 12 }}>
          {dash(v)}
        </Typography.Text>
      ),
    },
    {
      title: '推理后端',
      dataIndex: 'backends',
      key: 'backends',
      width: 140,
      render: (v: string[]) =>
        v && v.length ? (
          <Space size={4} wrap>
            {v.map((b) => (
              <Tag key={b} style={{ marginInlineEnd: 0 }}>
                {b}
              </Tag>
            ))}
          </Space>
        ) : (
          '—'
        ),
    },
    {
      title: '大小',
      dataIndex: 'file_size',
      key: 'size',
      width: 100,
      render: (v: number) => humanBytes(v),
    },
    {
      title: '附属物',
      dataIndex: 'manifest',
      key: 'manifest',
      width: 90,
      render: (v: ManifestItem[]) => (v && v.length ? `${v.length} 个` : '—'),
    },
    {
      title: '上传时间',
      dataIndex: 'uploaded_at',
      key: 'up',
      width: 170,
      render: (v: string | null) => (v ? formatDateTime(v) : '—'),
    },
    {
      title: '操作',
      key: 'action',
      width: 120,
      render: (_: unknown, r: ModelOut) => (
        <Space size={12}>
          <a onClick={() => setDetail(r)}>详情</a>
          <Popconfirm
            title="删除该模型？"
            description="将同时删除模型文件与全部附属物，已下发的节点不受影响。"
            onConfirm={() => remove(r.model_id)}
            okText="删除"
            cancelText="取消"
          >
            <a style={{ color: '#ff7875' }}>删除</a>
          </Popconfirm>
        </Space>
      ),
    },
  ]

  return (
    <Card
      title="模型库"
      size="small"
      style={CARD_STYLE}
      extra={
        <Space>
          <Button icon={<ReloadOutlined />} onClick={() => load(true)}>
            刷新
          </Button>
          <Button type="primary" icon={<UploadOutlined />} onClick={() => setOpen(true)}>
            上传模型
          </Button>
        </Space>
      }
    >
      <Alert
        type="info"
        showIcon
        style={{ marginBottom: 12 }}
        message="登记信息（量化 / 目标 SoC / QNN 版本 / 后端）是下发前兼容性检查的依据。"
        description="留空表示「未登记」——检查会如实标为 unknown 并放行，而不是当成不兼容。请勿为了好看填假值。"
      />
      <Table
        rowKey="model_id"
        size="small"
        loading={loading && models.length === 0}
        columns={columns as never}
        dataSource={models}
        pagination={{ pageSize: 10 }}
      />

      <UploadModal
        open={open}
        form={form}
        uploading={uploading}
        progress={uploadProgress}
        file={file}
        onFile={setFile}
        onCancel={() => {
          setOpen(false)
          setFile(null)
          setUploadProgress(null)
          form.resetFields()
        }}
        onOk={submit}
      />

      {detail && (
        <ModelDetail
          model={detail}
          nodes={nodes}
          onClose={() => setDetail(null)}
          onChanged={(m) => {
            setDetail(m)
            load(false)
          }}
        />
      )}
    </Card>
  )
}

// ===================================================================== 上传

function UploadModal(props: {
  open: boolean
  form: ReturnType<typeof Form.useForm>[0]
  uploading: boolean
  progress: number | null
  file: File | null
  onFile: (f: File | null) => void
  onCancel: () => void
  onOk: () => void
}) {
  const { open, form, uploading, progress, file, onFile, onCancel, onOk } = props
  return (
    <Modal
      title="上传模型"
      open={open}
      onCancel={onCancel}
      onOk={onOk}
      confirmLoading={uploading}
      okText="上传"
      cancelText="取消"
      width={720}
    >
      <Form form={form} layout="vertical" initialValues={{ version: 'v1', framework: 'other', self_test_timeout: 60 }}>
        <Space size={12} style={{ width: '100%' }}>
          <Form.Item name="name" label="名称" rules={[{ required: true, message: '请填写模型名称' }]} style={{ flex: 1 }}>
            <Input placeholder="如 llama-3.2-1b" />
          </Form.Item>
          <Form.Item name="version" label="版本" style={{ width: 140 }}>
            <Input />
          </Form.Item>
          <Form.Item name="framework" label="框架" style={{ width: 160 }}>
            <Select options={FRAMEWORKS} />
          </Form.Item>
        </Space>

        <Space size={12} style={{ width: '100%' }}>
          <Form.Item name="quantization" label="量化精度" style={{ flex: 1 }}>
            <Input placeholder="如 Q4_K_M / W8A8" />
          </Form.Item>
          <Form.Item name="target_soc" label="目标 SoC" style={{ flex: 1 }}>
            <Input placeholder="如 SM8550，多个用逗号分隔" />
          </Form.Item>
          <Form.Item name="qnn_version" label="QNN 版本" style={{ width: 160 }}>
            <Input placeholder="如 2.20" />
          </Form.Item>
        </Space>

        <Space size={12} style={{ width: '100%', alignItems: 'flex-start' }}>
          <Form.Item
            name="backends"
            label="推理后端（JSON 数组）"
            style={{ flex: 1 }}
            extra='如 ["htp","cpu"]'
          >
            <Input placeholder='["htp","cpu"]' />
          </Form.Item>
          <Form.Item name="deps" label="依赖（JSON 数组）" style={{ flex: 1 }} extra='如 ["libQnnHtp.so"]'>
            <Input placeholder='["libQnnHtp.so"]' />
          </Form.Item>
        </Space>

        <Form.Item name="run_params" label="运行参数（JSON 对象）" extra='如 {"n_ctx":2048,"threads":4}'>
          <Input placeholder='{"n_ctx":2048}' />
        </Form.Item>

        <Space size={12} style={{ width: '100%' }}>
          <Form.Item
            name="self_test_command"
            label="Self Test 命令"
            style={{ flex: 1 }}
            extra="部署落位后执行；可用 {model_path} 占位。留空跳过自检。"
          >
            <Input placeholder="如 llama-cli -m {model_path} -p hi -n 4" />
          </Form.Item>
          <Form.Item name="self_test_timeout" label="自检超时（秒）" style={{ width: 160 }}>
            <InputNumber min={1} max={3600} style={{ width: '100%' }} />
          </Form.Item>
        </Space>

        <Form.Item name="format" label="格式">
          <Input placeholder="如 gguf / onnx / bin" />
        </Form.Item>
        <Form.Item name="description" label="说明">
          <Input.TextArea rows={2} />
        </Form.Item>

        <Form.Item label="模型文件" required>
          <input
            type="file"
            onChange={(e) => onFile(e.target.files?.[0] ?? null)}
            style={{ color: '#e6edf3' }}
          />
          {file && (
            <Typography.Text type="secondary" style={{ fontSize: 12, marginLeft: 8 }}>
              {file.name}（{humanBytes(file.size)}）
            </Typography.Text>
          )}
          {uploading && progress !== null && (
            <Progress percent={progress} size="small" status="active" style={{ marginTop: 8 }} />
          )}
        </Form.Item>
      </Form>
    </Modal>
  )
}

// ===================================================================== 详情

function ModelDetail(props: {
  model: ModelOut
  nodes: NodeInfo[]
  onClose: () => void
  onChanged: (m: ModelOut) => void
}) {
  const { model, nodes, onClose, onChanged } = props
  const [editForm] = Form.useForm()
  const [saving, setSaving] = useState(false)
  const [pickedNodes, setPickedNodes] = useState<string[]>([])
  const [checking, setChecking] = useState(false)
  const [checks, setChecks] = useState<PrecheckNodeResult[] | null>(null)
  const [assetKind, setAssetKind] = useState('config')
  const [assetFiles, setAssetFiles] = useState<File[]>([])
  const [assetBusy, setAssetBusy] = useState(false)

  useEffect(() => {
    editForm.setFieldsValue({
      name: model.name,
      version: model.version,
      framework: model.framework,
      format: model.format,
      quantization: model.quantization,
      target_soc: model.target_soc,
      qnn_version: model.qnn_version,
      backends: model.backends?.length ? JSON.stringify(model.backends) : '',
      deps: model.deps?.length ? JSON.stringify(model.deps) : '',
      run_params:
        model.run_params && Object.keys(model.run_params).length
          ? JSON.stringify(model.run_params)
          : '',
      self_test_command: model.self_test_command,
      self_test_timeout: model.self_test_timeout,
      description: model.description,
    })
  }, [model, editForm])

  const save = async () => {
    let v: Record<string, unknown>
    try {
      v = await editForm.validateFields()
    } catch {
      return
    }
    let backends: unknown
    let deps: unknown
    let runParams: unknown
    try {
      backends = parseJsonField(String(v.backends ?? ''), 'list')
      deps = parseJsonField(String(v.deps ?? ''), 'list')
      runParams = parseJsonField(String(v.run_params ?? ''), 'dict')
    } catch (e) {
      message.error(`JSON 字段格式不对：${(e as Error).message}`)
      return
    }
    setSaving(true)
    try {
      // PATCH 语义：**只提交这里显式给出的字段**，没给的保持原样。
      // 把空串当成"清空"是刻意的 —— 用户把量化删掉，就是他要把这项改回"未登记"。
      const updated = await api.patchModel(model.model_id, {
        name: String(v.name ?? '').trim() || model.name,
        version: String(v.version ?? '').trim() || model.version,
        framework: (v.framework as Framework) ?? model.framework,
        format: String(v.format ?? ''),
        quantization: String(v.quantization ?? ''),
        target_soc: String(v.target_soc ?? ''),
        qnn_version: String(v.qnn_version ?? ''),
        backends: (backends as string[] | undefined) ?? [],
        deps: (deps as string[] | undefined) ?? [],
        run_params: (runParams as Record<string, unknown> | undefined) ?? {},
        self_test_command: String(v.self_test_command ?? ''),
        self_test_timeout: Number(v.self_test_timeout ?? 60),
        description: String(v.description ?? ''),
      })
      message.success('登记信息已更新')
      onChanged(updated)
    } catch (e) {
      message.error(errText(e, '保存失败'))
    } finally {
      setSaving(false)
    }
  }

  const runPrecheck = async () => {
    if (!pickedNodes.length) {
      message.warning('请先选择要检查的节点')
      return
    }
    setChecking(true)
    try {
      const out = await api.modelCompat(model.model_id, pickedNodes)
      setChecks(out.results)
    } catch (e) {
      message.error(errText(e, '预检失败'))
    } finally {
      setChecking(false)
    }
  }

  const uploadAssets = async () => {
    if (!assetFiles.length) {
      message.warning('请选择要上传的附属物文件')
      return
    }
    setAssetBusy(true)
    try {
      const updated = await api.uploadManifest(model.model_id, assetKind, assetFiles)
      message.success(`已上传 ${assetFiles.length} 个附属物`)
      setAssetFiles([])
      onChanged(updated)
    } catch (e) {
      message.error(errText(e, '附属物上传失败'))
    } finally {
      setAssetBusy(false)
    }
  }

  const removeAsset = async (name: string) => {
    try {
      const updated = await api.deleteManifest(model.model_id, name)
      message.success('已删除附属物')
      onChanged(updated)
    } catch (e) {
      message.error(errText(e, '删除失败'))
    }
  }

  const assetColumns = [
    {
      title: '文件',
      dataIndex: 'name',
      key: 'name',
      render: (v: string) => (
        <Typography.Text className="mono" style={{ fontSize: 12 }}>
          {v}
        </Typography.Text>
      ),
    },
    {
      title: '类型',
      dataIndex: 'kind',
      key: 'kind',
      width: 90,
      render: (v: string) => <Tag>{MANIFEST_KIND_META[v] ?? v}</Tag>,
    },
    {
      title: '大小',
      dataIndex: 'size',
      key: 'size',
      width: 100,
      render: (v: number) => humanBytes(v),
    },
    {
      title: '操作',
      key: 'action',
      width: 110,
      render: (_: unknown, r: ManifestItem) => (
        <Space size={12}>
          <a href={manifestDownloadUrl(model.model_id, r.name)} target="_blank" rel="noreferrer">
            下载
          </a>
          <Popconfirm title="删除该附属物？" onConfirm={() => removeAsset(r.name)} okText="删除" cancelText="取消">
            <a style={{ color: '#ff7875' }}>删除</a>
          </Popconfirm>
        </Space>
      ),
    },
  ]

  const checkColumns = [
    { title: '维度', dataIndex: 'name', key: 'name', width: 110, render: (v: string) => v || '—' },
    {
      title: '结论',
      dataIndex: 'verdict',
      key: 'verdict',
      width: 90,
      render: (v: CompatVerdict) => {
        const m = VERDICT_META[v] ?? VERDICT_META.unknown
        return <Tag color={m.color}>{m.text}</Tag>
      },
    },
    { title: '要求', dataIndex: 'want', key: 'want', width: 160, render: dash },
    { title: '实际', dataIndex: 'got', key: 'got', width: 180, render: dash },
    { title: '说明', dataIndex: 'detail', key: 'detail' },
  ]

  return (
    <Drawer
      title={`模型详情：${model.name} ${model.version}`}
      open
      onClose={onClose}
      width={860}
      styles={{ body: { background: '#14181f' } }}
    >
      <Descriptions
        size="small"
        column={2}
        bordered
        title="文件"
        items={[
          { key: 'id', label: 'model_id', children: <span className="mono">{model.model_id}</span> },
          { key: 'file', label: '文件名', children: <span className="mono">{model.file_name}</span> },
          { key: 'size', label: '大小', children: humanBytes(model.file_size) },
          { key: 'dc', label: '下载次数', children: model.download_count ?? 0 },
          {
            key: 'sha',
            label: 'SHA-256',
            span: 2,
            children: (
              <Typography.Text className="mono" style={{ fontSize: 11 }} copyable={{ text: model.sha256 }}>
                {model.sha256 || '—'}
              </Typography.Text>
            ),
          },
          {
            key: 'dl',
            label: '下载',
            span: 2,
            children: (
              <a href={modelDownloadUrl(model.model_id)} target="_blank" rel="noreferrer">
                直接下载模型文件
              </a>
            ),
          },
        ]}
      />

      <Divider orientation="left" style={{ marginTop: 24 }}>
        登记信息（需求 §5）
      </Divider>
      <Form form={editForm} layout="vertical">
        <Space size={12} style={{ width: '100%' }}>
          <Form.Item name="name" label="名称" style={{ flex: 1 }}>
            <Input />
          </Form.Item>
          <Form.Item name="version" label="版本" style={{ width: 120 }}>
            <Input />
          </Form.Item>
          <Form.Item name="framework" label="框架" style={{ width: 150 }}>
            <Select options={FRAMEWORKS} />
          </Form.Item>
        </Space>
        <Space size={12} style={{ width: '100%' }}>
          <Form.Item name="quantization" label="量化精度" style={{ flex: 1 }}>
            <Input />
          </Form.Item>
          <Form.Item name="target_soc" label="目标 SoC" style={{ flex: 1 }}>
            <Input placeholder="SM8550，多个用逗号分隔" />
          </Form.Item>
          <Form.Item name="qnn_version" label="QNN 版本" style={{ width: 150 }}>
            <Input />
          </Form.Item>
        </Space>
        <Space size={12} style={{ width: '100%' }}>
          <Form.Item name="backends" label="推理后端（JSON 数组）" style={{ flex: 1 }}>
            <Input placeholder='["htp","cpu"]' />
          </Form.Item>
          <Form.Item name="deps" label="依赖（JSON 数组）" style={{ flex: 1 }}>
            <Input placeholder='["libQnnHtp.so"]' />
          </Form.Item>
        </Space>
        <Form.Item name="run_params" label="运行参数（JSON 对象）">
          <Input placeholder='{"n_ctx":2048}' />
        </Form.Item>
        <Space size={12} style={{ width: '100%' }}>
          <Form.Item name="self_test_command" label="Self Test 命令" style={{ flex: 1 }} extra="可用 {model_path} 占位">
            <Input />
          </Form.Item>
          <Form.Item name="self_test_timeout" label="超时（秒）" style={{ width: 130 }}>
            <InputNumber min={1} max={3600} style={{ width: '100%' }} />
          </Form.Item>
        </Space>
        <Form.Item name="format" label="格式">
          <Input />
        </Form.Item>
        <Form.Item name="description" label="说明">
          <Input.TextArea rows={2} />
        </Form.Item>
        <Button type="primary" onClick={save} loading={saving}>
          保存登记信息
        </Button>
        <Typography.Text type="secondary" style={{ marginLeft: 12, fontSize: 12 }}>
          保存是增量的：清空某一项即把它改回「未登记」。
        </Typography.Text>
      </Form>

      <Divider orientation="left" style={{ marginTop: 24 }}>
        兼容性预检（需求 §5）
      </Divider>
      <Space style={{ marginBottom: 12 }} wrap>
        <Select
          mode="multiple"
          style={{ minWidth: 380 }}
          placeholder="选择要检查的节点"
          value={pickedNodes}
          onChange={setPickedNodes}
          options={nodes.map((n) => ({
            value: n.node_id,
            label: `${n.name || n.node_id}（${n.soc || 'SoC 未知'}）`,
          }))}
        />
        <Button onClick={runPrecheck} loading={checking}>
          预检
        </Button>
      </Space>
      {checks === null ? (
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          预检与真正下发用的是同一套判定逻辑，因此这里的结果就是下发的结论。
        </Typography.Text>
      ) : checks.length === 0 ? (
        <Empty description="没有结果" />
      ) : (
        <Space direction="vertical" style={{ width: '100%' }} size={12}>
          {checks.map((r) => (
            <div key={r.node_id}>
              <Space size={8}>
                <strong>{r.node_name || r.node_id}</strong>
                {r.ok ? <Tag color="success">可下发</Tag> : <Tag color="error">会被拦截</Tag>}
                {!r.ok && r.reason ? (
                  <Typography.Text type="danger" style={{ fontSize: 12 }}>
                    {r.reason}
                  </Typography.Text>
                ) : null}
                {r.ok && r.checks?.every((c: CompatCheck) => c.verdict === 'unknown') ? (
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    所有维度都未登记：按「拿不到 ≠ 不兼容」放行
                  </Typography.Text>
                ) : null}
              </Space>
              <Table
                rowKey="name"
                size="small"
                style={{ marginTop: 6 }}
                pagination={false}
                columns={checkColumns as never}
                dataSource={r.checks ?? []}
              />
            </div>
          ))}
        </Space>
      )}

      <Divider orientation="left" style={{ marginTop: 24 }}>
        部署包附属物（Deployment Package，需求 §6）
      </Divider>
      <Space style={{ marginBottom: 12 }} wrap align="end">
        <div>
          <div style={{ fontSize: 12, marginBottom: 4 }}>类型</div>
          <Select style={{ width: 130 }} value={assetKind} onChange={setAssetKind} options={MANIFEST_KINDS} />
        </div>
        <div>
          <div style={{ fontSize: 12, marginBottom: 4 }}>文件（可多选，同名覆盖）</div>
          <input
            type="file"
            multiple
            onChange={(e) => setAssetFiles(Array.from(e.target.files ?? []))}
            style={{ color: '#e6edf3' }}
          />
        </div>
        <Button type="primary" onClick={uploadAssets} loading={assetBusy} icon={<UploadOutlined />}>
          上传
        </Button>
      </Space>
      <Table
        rowKey="name"
        size="small"
        pagination={false}
        columns={assetColumns as never}
        dataSource={model.manifest ?? []}
        locale={{ emptyText: <Empty description="没有附属物（部分模型不需要）" /> }}
      />
      <Tooltip title="附属物在模型主体落位后由 Agent 依次拉取；拉取失败只告警不判失败，因为有些附属物是可选的。">
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          提示：附属物随模型一起下发，缺一个可选配置不会让整次部署失败。
        </Typography.Text>
      </Tooltip>
    </Drawer>
  )
}
