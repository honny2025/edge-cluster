# C 盘瘦身与迁移到 E 盘指南

> 实测时间：2026-10-05　|　本机三块盘：**C: 200G（只剩 24G）/ D: 100G（剩 33G）/ E: 176G（剩 46G）**

---

## 〇、先说结论

**这个平台不占 C 盘。** 整个 edge-cluster 项目在 `E:\Users\<用户名>\Desktop\edge-cluster-latest`，
它的虚拟环境（`.venv`）、数据库（`server/data/`）、日志、模型全都在项目目录里，也就是都在 E 盘。
它往 C 盘写的只有 pip 缓存 28 MB 和几 MB 的临时文件 —— 可以忽略。

C 盘只剩 24G **不是这个项目造成的**。真正吃掉空间的是这三样：

| 排名 | 位置 | 占用 | 一句话 |
|---|---|---|---|
| 1 | `C:\Users\<用户名>\WPSDrive\...\WPS企业云盘` | **72.1 G** | WPS 企业云盘的**本地同步副本**，云端有原件 |
| 2 | `C:\Users\<用户名>\AppData` | **69 G** | 各种软件的数据与缓存（Local 48G + Roaming 21G） |
| 3 | `C:\Users\<用户名>\.workbuddy` | **7.7 G** | 里面 1.7G 是日志，可以直接删 |

**光第 1 项处理掉，C 盘就从「只剩 24G」变成「只剩 96G」。** 这是唯一一笔值得先动的。

---

## 一、C 盘到底被谁占了（实测明细）

整个用户目录 `C:\Users\<用户名>` 实测 **167 G**，构成如下：

| 目录 | 大小 | 是什么 |
|---|---|---|
| `WPSDrive` | **72.1 G** | WPS 企业云盘本地同步目录 |
| `AppData` | **69 G** | 软件数据（下面是明细） |
| `.workbuddy` | 7.7 G | 某个应用的 workspace / logs / binaries |
| `Documents` | 4.6 G | 我的文档（含微信/QQ 默认接收目录等） |
| `.cache` | 3.2 G | 各种工具的缓存 |
| `.android` | 2.6 G | Android 模拟器镜像（AVD） |
| `WorkBuddy` | 1 G | 同上 |
| `.vscode` | 1 G | VSCode 扩展 |
| `.codex` / `.dsh` / `.qwenworkcn` | 约 2 G | 几个 AI 工具的本地数据 |
| 其余 | 约 3 G | 零碎 |

### AppData 明细（69 G 的大头）

`AppData\Local`（48 G，其中约 11 G 其实在 E 盘，见下）：

| 子目录 | 大小 | 说明 |
|---|---|---|
| `Programs` | 12.1 G | **用户级安装的软件**：Python 3.5G、PyCharm 3.2G、Ollama 2.7G… |
| `Docker\wsl` | 11.0 G | Docker Desktop 的 WSL 磁盘镜像 |
| `Arduino15` | （11 G） | **已经是 E 盘的软链接** → 实际在 `E:\Arduino15`，不占 C 盘 |
| `kingsoft` | 3 G | WPS 程序数据 |
| `Microsoft` / `Packages` | 各 2 G | 微软组件、UWP 应用数据 |
| `OpenAI` | 1 G | OpenAI 相关工具 |
| `Temp` | 866 M | 临时文件，**可删** |
| `npm-cache` / `pnpm-cache` | 433 + 309 M | 包管理器缓存，**可删** |
| `pip\cache` | 28 M | pip 缓存，**可删** |

`AppData\Roaming`（21 G）：`kingsoft` 5G、`TRAE SOLO CN` 4G、`Tencent` 2G、`baidu` 2G、
`Code` 2G、`Python` 1G —— 全是各软件自己的数据目录。

> **注意**：上面 `Arduino15` 那 11 G 是我第一次扫的时候重复计进去的。
> 它其实是个软链接（junction），真身在 `E:\Arduino15`。
> **这说明你之前已经成功做过一次「搬走 + 建软链接」，方法本身是可行的** —— 下面第 3 节讲的也是这个办法。

---

## 二、哪些能搬上 E 盘，哪些别碰

### ✅ 可以搬（按"省得多 + 风险低"排序）

| 优先级 | 目标 | 省 | 怎么搬 | 风险 |
|---|---|---|---|---|
| **1** | `WPSDrive\...\WPS企业云盘` **72 G** | ★★★★★ | 见下方 **第 3 节**，改 WPS 自己的设置 | 低（云端有原件） |
| 2 | `AppData\Local\Docker\wsl` 11 G | ★★★★ | 见 **第 4 节** | 低 |
| 3 | `AppData\Local\Programs` 12 G | ★★★★ | **不要搬，要重装到 E 盘**（见第 5 节） | 中 |
| 4 | `.workbuddy\logs` 1.7 G | ★★ | 直接删（纯日志） | 无 |
| 5 | `AppData\Local\Temp` 866 M | ★ | 直接删 | 无 |
| 6 | `npm-cache` + `pnpm-cache` + `pip\cache` 770 M | ★ | 直接删（下次装包会重建） | 无 |
| 7 | `.cache` 3.2 G | ★★ | 里面有真东西的话先看一眼，多数可删 | 低 |
| 8 | `Documents` 4.6 G | ★★ | 右键 → 属性 → **位置** → 移动 → 选 E 盘 | 低（Windows 官方功能） |
| 9 | `.android` 2.6 G | ★★ | Android Studio → 设备管理器里改 AVD 存放路径 | 低 |

### ⛔ 别手工搬（会出问题）

| 目标 | 为什么 |
|---|---|
| `AppData\Roaming` 下任何东西（21 G） | 软件把路径**写死在注册表和配置文件里**，拖走之后程序会找不到自己的数据，表现是"设置全丢了"甚至打不开。想省空间只能改**软件自己的设置**（例：VSCode 扩展目录、腾讯系软件的存储位置） |
| `AppData\Local\Programs` 里的软件 | 同上。**正确做法是卸载后重新安装到 E 盘**，不是剪切粘贴 |
| 直接剪切 `AppData` 任何子目录到 E 盘 | 同上。真要用链接，也必须走下面第 6 节的 `mklink /J` |

---

## 三、最大的一笔：WPS 云盘 72 G

`C:\Users\<用户名>\WPSDrive\<账号ID>\WPS企业云盘` 是 WPS 云盘的**本地同步副本**。
里面 72 G 的文件在 WPS 云端都有原件，本地这份只是缓存的镜像。

**两条路，选一条：**

### 路线 A（推荐）：改成"按需下载"，本地几乎不占空间

WPS 云盘支持"在线文件"模式 —— 文件仍出现在资源管理器里，但**点开才下载**。
这样 72 G 的占用能降到接近 0，而且不依赖 E 盘有没有空间。

做法：打开 WPS → 左侧「WPS 云盘 / 企业云盘」→ 右上角**设置（齿轮）** →
找到 **「本地文件夹位置」/「按需下载」/「仅显示在线文件」** 一类的开关 →
关闭"始终保留在此设备"或打开"按需下载"。

> 各家 WPS 版本菜单叫法不同，找不到就把这一整段话搜一下：**「WPS 云盘 本地缓存 位置 修改」**。

### 路线 B：把本地目录整个挪到 E 盘

在同一个设置页里，把「本地文件夹位置」从
`C:\Users\<用户名>\WPSDrive` 改成 `E:\WPSDrive`，WPS 会自己迁移并更新指向。

> ⚠️ **注意 E 盘只有 46 G 空闲，装不下 72 G。**
> 要走这条路，得先把 WPS 里"企业云盘"和"云盘"分开设到 E: 和 D: 两个位置，
> 或者先做路线 A 把那 72 G 缩下来，再挪。
>
> **建议：先做路线 A**。云端有原件的东西，没必要在本地留一份。

---

## 四、Docker 的 11 G

`C:\Users\<用户名>\AppData\Local\Docker\wsl` 是 Docker Desktop 用的 WSL2 磁盘镜像，
里面装的是你所有容器和镜像。

**做法**：Docker Desktop → 设置 → **Resources → Advanced** →
「Disk image location」改到 `E:\Docker`，点 Apply，Docker 会自己搬。

如果设置里找不到这个选项，就用 WSL 官方导入导出：

```powershell
wsl --shutdown
wsl --export docker-desktop-data E:\docker-data.tar
wsl --unregister docker-desktop-data
wsl --import docker-desktop-data E:\Docker\data E:\docker-data.tar
```

> 做之前确认 E 盘有 11 G 以上空闲（现在有 46 G，够）。
> 搬完那个 `.tar` 备份文件确认没问题后再删。

---

## 五、`AppData\Local\Programs` 里的 12 G 软件

这里面是**装在用户目录下的软件**：

| 软件 | 大小 |
|---|---|
| Python | 3.5 G |
| PyCharm 2025.3.1.1 | 3.2 G |
| Ollama | 2.7 G |
| 其他（DeepSeek Harness、微云、Quark、nodejs 等） | 约 2.7 G |

**❌ 不要剪切粘贴**。这些软件安装时把绝对路径写进了注册表，
拖走之后会变成"快捷方式全失效、程序起不来"。

**✅ 正确做法**：

1. 控制面板 → 卸载该软件
2. 重新安装，**在安装向导里把路径改成 `E:\Programs\XXX`**

其中最值得动的是 **Python 3.5 G** 和 **PyCharm 3.2 G**（重装能直接选路径）。
Ollama 重装后模型目录默认在 `C:\Users\<用户名>\.ollama` ——
本机实测**没有这个目录**，说明模型存在别处，搬之前先在 Ollama 设置里确认模型路径。

---

## 六、通用兜底办法：搬走 + 建目录链接（junction）

如果某个目录**既不能在软件设置里改位置，又确实很大**，可以用这招：
把真身放到 E 盘，在原位置留一个"传送门"，程序完全察觉不到。

**你已经成功用过一次了** —— `AppData\Local\Arduino15` 就是个指向 `E:\Arduino15` 的链接。

**步骤**（必须以**管理员身份**打开 PowerShell 或 CMD）：

```cmd
:: 1. 先关掉要用到它的程序
:: 2. 把目录整个搬到 E 盘
robocopy "C:\Users\<用户名>\目标目录" "E:\目标目录" /E /MOVE /COPYALL

:: 3. 在原位置建链接（/J = 目录联接，最稳，不需要开启开发者模式）
mklink /J "C:\Users\<用户名>\目标目录" "E:\目标目录"
```

**验证**：回到资源管理器打开原路径，能看到文件，图标上带个小箭头 → 成功。

**注意事项**：

- `mklink /J` 是**目录**链接；不要用 `/D`（那是符号链接，权限要求更高）
- 用 `robocopy /MOVE` 而不是剪切粘贴：断在大文件上也能续
- **不要对 `AppData\Roaming` 整个做这个** —— 有些程序会检测到链接后拒绝启动
- 单个目录做之前，先只做一个，重启确认没问题再做下一个

---

## 七、零风险、立刻能做的清理

下面这些删了**不会有任何副作用**（都是缓存或日志），不用等，也不用搬：

| 路径 | 能省 | 说明 |
|---|---|---|
| `C:\Users\<用户名>\.workbuddy\logs` | 1.7 G | 纯日志 |
| `C:\Users\<用户名>\AppData\Local\Temp` | 866 M | 临时文件（有程序占用会跳过，正常） |
| `C:\Users\<用户名>\AppData\Local\npm-cache` | 433 M | 下次 `npm install` 会重建 |
| `C:\Users\<用户名>\AppData\Local\pnpm-cache` | 309 M | 同上 |
| `C:\Users\<用户名>\AppData\Local\pip\cache` | 28 M | 下次 `pip install` 会重建 |
| Windows 磁盘清理 / 存储感知 | 不定 | 设置 → 系统 → 存储 → 清理建议 |

**合计约 3.3 G。**

---

## 八、建议的执行顺序

```
第 1 步  删日志和缓存                          → 省 3.3 G   （零风险，五分钟）
第 2 步  WPS 云盘改「按需下载」（第 3 节路线 A） → 省最多 72 G（最大的一笔）
第 3 步  Docker 镜像挪到 E 盘（第 4 节）        → 省 11 G
第 4 步  Documents 改位置到 E 盘（第 2 节）     → 省 4.6 G
第 5 步  .cache / .android 清理或改路径          → 省 3～6 G
第 6 步  Python / PyCharm 重装到 E 盘           → 省 6.7 G
```

**做完第 2 步，C 盘就已经安全了。** 后面的都是锦上添花。

---

## 九、和这个平台的关系（重要）

再强调一遍，免得你误以为要清 C 盘才能跑平台：

- 平台代码、虚拟环境、数据库、日志、模型 —— **全部在 E 盘的项目目录里**
- 平台不会把数据写到 `C:\Users\<用户名>\AppData` 或 `Documents`
- 唯一往 C 盘写的是 `pip\cache`（28 MB）和系统临时文件，清不清都不影响

**所以：C 盘瘦身可以随时做、慢慢做，不影响平台运行。**
两件事互不相干。
