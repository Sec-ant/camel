# camel

Rust 漫画翻译库与 CLI。图片处理在本地执行，翻译通过兼容 Chat Completions 的 API 完成。字体和模型独立于程序安装。

可将常用设置放在 `$XDG_CONFIG_HOME/camel/config.json`（默认 `~/.config/camel/config.json`），或通过 `--config` 指定。命令行参数及对应环境变量优先于配置文件；配置中的相对路径以配置文件所在目录为基准。

```json
{
  "models": "models",
  "font": "fonts/LXGWWenKai-Medium.ttf",
  "jobs": 2,
  "translation": {
    "base_url": "https://example.com/v1",
    "model": "model-name",
    "target_language": "zh-CN",
    "prompt": "prompt.txt",
    "variables": {"series": "Series title"},
    "request_options": {}
  }
}
```

`translation` 还可设置 `fallback_model` 和 `api_key_file`。API 密钥保存在独立文件中，默认读取同一配置目录下的 `api-key`；也可通过 `CAMEL_API_KEY` 提供。

## 使用

安装 Rust，准备下表中的模型和中文字体，然后运行：

```sh
just install
camel analyze comic.cbr -o book --models models/
camel translate book \
  --base-url https://token-plan-cn.xiaomimimo.com/v1 \
  --model mimo-v2.6-pro
camel export book -o translated.cbz --font /path/to/comic.ttf
```

## 校对工作台

工作台是纯静态网页，核心库编译为 WebAssembly 在浏览器的 Web Worker 中运行，不需要后台服务。`just web-dev` 构建后在 http://localhost:5173 打开；构建产物 `web/dist/` 也可放在任意静态托管上。

打开时依次选择项目目录、原漫画（CBZ/ZIP、CBR/RAR 文件或图片目录）和中文字体。每页读取时按项目记录核对原图内容，选错漫画会直接提示。工作台可以：

- 逐页查看原图、擦字和成品三种画面，叠加区域编号、擦除掩码、排版区、OCR 词框、未识别候选和字形；预览与 `camel export --allow-partial` 的结果一致，并列出保留原文的区域及原因。
- 修改译文（`\n` 为手动换行）、校正 OCR 原文、设置字号，标记区域或整页保留原文。
- 用画笔增减擦除掩码和排版区，改用纯色填充（可吸取页面颜色），框选新建区域、把未识别候选转为区域、删除误识别区域。
- 每页撤销/重做；改动原文、保留标记或区域增删时，同步更新翻译检查点、上下文和 `translation-plan.json`。

Chromium 内核浏览器（Chrome、Edge 等）直接读写项目目录，并在几秒内显示 `camel translate` 等外部写入；两边改动同一页时，后写入的一方会发现冲突，工作台重新载入该页，`translate` 保留工作台的改动，该页留待下次补译。Firefox 和 Safari 以只读方式载入项目，改动保存在浏览器中，用“Download changes”下载只含改动文件的 zip，解压覆盖到项目目录。

`analyze` 接受 CBR/RAR、CBZ/ZIP、PNG、JPEG 或图片目录，按文件名中的数字排序。默认两个页线程共享模型权重；`--jobs N` 控制同时处理的页数。项目保存源路径、页内容校验值、区域坐标、擦字掩码和译文，不复制原图。分析中断后可重新运行同一命令继续；`inspect book` 输出 JSON 概况，包括已完成、部分完成、待处理、没有待翻译区域、保留原图、存在问题及受内容过滤影响的页数。诊断信息写入 stderr，`--quiet` 关闭进度输出。

识别算法更新后，可用 `camel refresh book new-analysis -o updated-book` 合并新识别结果与已有译文。命令按原文或校正后的原文及位置对应区域，保留匹配的译文和上下文；无法匹配的旧译文写入 `unmatched-translations.json`，供审校使用。

将下列 ONNX 文件放入同一目录。模型目录依次取自 `--models`、`CAMEL_MODELS`、`$XDG_DATA_HOME/camel/models`；未设置 XDG 数据目录时使用 `~/.local/share/camel/models`。

| 文件名 | 模型 |
| --- | --- |
| `detector.onnx` | [comic-text-and-bubble-detector 的 detector-v4-s_int8](https://huggingface.co/ogkalu/comic-text-and-bubble-detector) |
| `recognizer.onnx` | [RapidOCR 的 en_PP-OCRv5_rec_mobile](https://github.com/RapidAI/RapidOCR/blob/main/python/rapidocr/default_models.yaml)，含内嵌字符字典 |

检测与英文识别通过 RTen 在 Rust 进程内执行。模型独立于二进制安装，加载失败时报告所需文件及目录。`analysis.visual_detections` 保留检测结果，`analysis.ocr_engine` 标注识别分数的来源。对易混淆的首字形，识别结果结合本页其他字形比对；原始 OCR 保留在 `analysis.initial_ocr`，修正后的文字进入页面区域。

检测到文字但未能分行时，会尝试对整块区域补充识别；所得候选保存在 `analysis.ocr_alternatives`，与原检测坐标关联。此结果未经其他识别结果印证，不自动用于擦字或翻译。

`analysis.ocr_observations` 将原始文字检测 `detection_id`、分行坐标 `lines` 和阅读排序后的 `initial_ocr` 块 `block_id` 关联起来。分行已找到但未读出的行仍保留坐标；逐词的 `line` 对应此处的行索引。没有形成初始 OCR 块时省略 `block_id`。去重后移除的观察仍可追溯，待处理区域通过 `detection_id` 指回原始检测框。

气泡内没有文字检测框时，会检查气泡中的文字线。三个独立、对齐的点可识别为省略号，所对应的行索引保存在 `ocr_observations[].geometric_lines`，其词置信度为 0，表示没有模型分数。

`export --clean` 输出清除原文后的图片；目录输出包含 PNG 和同名 JSON，`.cbz` 输出为漫画文件。这些文件可用于检查掩盖效果、手工填字或交给其他排版工具。目录输出可通过 `--mask-output` 另外输出黑白掩码：白色像素是擦除区域，黑色像素保持原样。擦字导出只需要分析结果。重跑时会复用与当前结果完全相同的文件并补齐缺失文件；遇到内容不同的已有文件会保留并提示。图像尺寸与原图一致，区域坐标可以直接复用。`export` 默认使用同一份掩码擦字，再排入项目中的译文；输出路径以 `.cbz` 结尾时逐页写入 CBZ，否则写入图片目录。重跑时保留内容完全相同的已生成图片和报告，补齐缺失文件；内容不同则提示。

程序也可以通过 `geometry::prepare_erase(&original, &bubble)?.layer()` 按区域生成透明 RGBA 擦字图层及其原图坐标；叠回原图的效果与直接擦字一致。图层按需在内存中生成，项目仍以原图、掩码和背景重建参数为准。

Rust 库的 `comic::ComicSource` 可直接读取图片目录、CBZ/ZIP 和 CBR/RAR 中的单页。

JSON 使用原图像素坐标，原点在左上角，矩形右边和下边为开区间。`page.regions` 保存原文、逐词坐标与置信度、译文和背景色。其中 `bubble.glyphs` 是原文字形掩码，`bubble.erase` 是实际擦除区，`bubble.interior` 是检测估计的安全排版区。渐变纸面另存 `background_model` 或 `background_repair`，供擦字与排版从原图重建相同背景。`analysis` 保存有序画格、对白与画格关联、几何处理前的 OCR，以及安全排版区的闭合像素轮廓（可含孔洞和分离区域）；这些轮廓不是气泡外沿的精确描边。`analysis.unresolved_regions` 保存待处理文字的逐词坐标、置信度和原因。模型修正原文、说话者描述和场景摘要另存，并注明来源。

`interior` 和 `erase` 中的水平像素段以 `[y, start, end]` 存储，`end` 为开区间。字形掩码 `glyphs` 较为密集，按坐标分别做差分、ZigZag 和变长整数编码，再以 zlib 压缩、Base64 表示；空掩码省略。

局部纸面重建在 `background_repair.method` 中标记为 `harmonic_local_v1`；该区域的原图、擦除掩码和排版区均可用于人工检查。

API 密钥从 `CAMEL_API_KEY` 或 `~/.config/camel/api-key` 读取。可用 `--api-key-file` 指定其他文件。Base URL、模型、字体分别支持 `CAMEL_BASE_URL`、`CAMEL_MODEL`、`CAMEL_FONT`。MiMo Token Plan 的地址以控制台提供的 OpenAI 兼容地址为准。

翻译按页保存。重新执行同一命令会恢复术语表和前文，只补译 `null` 或空白台词；已有译文作为同页上下文保留。默认向视觉模型发送长边最多 1600 像素的页面 JPEG、原图尺度的区域坐标、当前页对白、下一页原文预览，以及最近的翻译和场景摘要；整页缩小后难以辨认的文字会附带标明区域的原图局部，每次最多六处。原文忽略英文大小写和多余空白后相同时，也提供少量先前译法作为参考，由模型结合当前语境决定是否复用。`--text-only` 使用纯文本模型。返回内容经过区域 ID 和术语格式校验后一起落盘。项目引用原漫画文件，不复制原图。

默认不写入翻译响应缓存。需要复用完全相同的请求时可使用 `--cache`；缓存位于项目的 `.cache/translation/`，不保存请求或密钥。

临时网络错误及 HTTP 408/429/500/502/503/504 每个模型最多尝试三次；短 `Retry-After` 会被遵守，超过 30 秒或日期形式的服务端等待要求则结束本次运行，之后可继续。可用 `--fallback-model mimo-v2.6-flash`（或 `CAMEL_FALLBACK_MODEL`）指定备用模型；首选模型重试耗尽或返回长度截断状态时使用。服务明确返回内容过滤时记录拒绝，不切换备用模型。认证错误、重定向和内容校验失败会直接报错。每页仍优先使用 `--model`；`translation_models` 逐条记录实际产出译文的模型，人工改动或清空该条台词后移除对应记录。未通过校验的回复不写入项目。

服务商特有的请求字段可放在配置文件的 `translation.request_options` 中。例如，支持该字段的 MiMo 模型可使用 `{"thinking":{"type":"disabled"}}` 关闭深度思考；这通常会减少延迟和输出 token，但可能影响复杂页面的理解质量。

翻译时可加 `--continue-on-error`：模型回复的内容或区域 ID 校验失败时，先重试一次该页；仍失败则记录问题并继续后续页面。临时服务错误在重试耗尽后也会标记当前页并继续；连续三页遇到临时服务错误、认证失败或服务要求较长等待时停止。服务明确拒绝时记录受影响的页面，继续处理时隔离相关页面和旧上下文。已有译文可继续使用；尚未完成的页面会在结果中报告。本次命令有未完成页面时返回非零状态，普通校验及临时错误可重新运行补译。

译文与原文完全相同时直接保留原图中的文字，避免无意义的擦除和重绘。识别到文字但无法生成字形掩码时保留原图并记录原因。需要先导出尚未全部翻译的项目时，可在 `export` 后加 `--allow-partial`：缺译或无法排版的文字保留原文；若其他气泡的擦除或排版区与保留区域相交，也保留对应原文。其他无法排版的情况保留整页原图。内容过滤历史写入报告，页面已有的译文照常处理；缺译区域按上述规则处理。这些情况会输出警告，并在输出目录的 `render-report.json` 中记录逐页、逐区域的原因；CBZ 输出时默认使用旁边的 `<漫画名>.report.json`；可通过 `--report` 指定其他报告路径。

已确认译名可以通过 `translate --glossary terms.json` 提供，例如 `{"Omni-Man":"全能侠"}`。术语表保存为项目内的 `glossary.json`，后续翻译自动读取，优先于模型建议和旧上下文；模型新提议另存为 `proposed_terms`；若建议与确认译名冲突，保留确认译名和对白，并在页面记录和运行日志中提示，供后续审校。每次请求仅发送当前页、后页预览和前文相关的术语，无关词条不占请求空间；可选部分合计最多 4096 字符，当前页命中的确认译名全部带入。更新术语用于后续请求，已有译文通过 `review` 审校修改。

可运行 `camel check-terms book -o terms-review.json`，在本地根据项目中的 `glossary.json` 检查已译对白。报告列出原文匹配词条但译文保留原文或缺少确认译名的候选，包括页名、区域 ID、原文、译文、词条及预期译名；明确保留的页面和区域不参与检查。普通词义、自然省略和识别错误等可能产生误报或漏报，需结合上下文核对后再用 `review` 和 `apply-review` 修改译文。发现候选时命令仍正常退出，也不影响导出；报告不会覆盖已有文件。

需要并发翻译时使用 `camel translate book --jobs 3`，也可在配置的 `translation.jobs` 中设置；默认仍为顺序翻译。首次并发处理会使用 OCR 原文生成 `translation-plan.json`，也可提前运行 `camel prepare book`，检查或调整场景分组与摘要。规划按有界文本批次处理，并使用相同的并发数；独立运行时可用 `camel prepare book --jobs 3`。跨批连续的场景会合并；场景之间并发、场景内部逐页顺序执行。分组和摘要是模型对原文的推断，跨场景请求不会等待前一场景的新译文，适合在审校时核对场景衔接。通过审校和区域修复命令更改保留标记或原文时，相关场景计划会同步更新；译文修改不会使计划失效。各场景持有独立上下文，跨场景候选译名冲突记录到页面 `issues`，供后续审校。续跑时按顺序恢复同场景内未受过滤影响的已译对白，遇到过滤标记页清空上下文；旧检查点中来源不明的候选词不参与恢复。

每次服务请求的耗时、实际模型、尝试次数、缓存命中、用量和错误写入项目的 `translation-metrics.jsonl`；日志不保存请求正文、图像或 API 密钥。`elapsed_ms` 是单次 API 尝试的总耗时，不含页图准备和保存，也不区分服务排队与模型生成。

初译后可运行 `camel audit book -o audit.json`，按场景组织双语审校，每组最多四页，并附邻页原文；可用 `--jobs 3` 并发审校，默认按顺序审校；若翻译使用自定义提示词，可通过 `--prompt prompt.txt` 让审校参考相同的翻译策略。没有计划时按相邻页面分组。审校只报告有原文依据的问题，将建议保存在 `audit_issues`；`pages[].regions[].translation` 保留当前译文。核对建议后，将接受的文字填到相应 `translation`，再运行 `camel apply-review book audit.json`。导入沿用原文、区域 ID 和修改冲突检查。已通过校验的审校响应缓存可在失败后复用。审校收到内容过滤时会记录相关页面并跳过可能包含相同内容的后续组；可通过重复使用 `--skip-page` 显式排除页面。审校回复两次校验失败时继续检查其他组，并将失败页面写入部分报告；命令返回错误，提示使用新的输出路径重跑失败组。审校请求有总字符上限；超大页面会明确报错，不截掉被审台词。

排版前可运行 `camel check-layout book -o layout.json`（可加 `--font comic.ttf`）。它使用与导出相同的字体、词组提示、原文字高和气泡形状，报告字号、放不下、缺字、缺译及重叠等问题；有内容过滤历史的页面同样检查已有译文，历史原因保留在报告中；源图等实际错误仍会使命令失败。可以据此只精简放不下的气泡，核对原意后重新检查；最终成图仍需核对擦字和画面效果。审校与排版报告均不覆盖已有输出文件。

使用 `translate --prompt prompt.txt` 可替换翻译提示词。模板支持 `{{target_language}}`、`{{style}}`、`{{visual}}`、`{{response_schema}}`、`{{context}}` 和 `{{terminology}}`；后两者分别加入前后文和相关词表，未引用时不发送对应内容。自定义模板缺少 `{{terminology}}` 时，启动翻译会提示本次不发送词表、不收集候选术语，并继续执行。`translation.variables` 可定义其他字符串变量，如 `{{series}}`，变量值按原文插入，不再递归展开。`{{style}}` 同时启用相应的本地对白格式处理；不引用时保留模型输出的标点。当前页面对白、坐标以及选用的上下文作为独立用户消息发送，模型输出仍按区域 ID 校验，页面记录保存提示词及变量的指纹。译文可以通过 `review` 导出、审校，再用 `apply-review` 写回。

例如，下面的模板使用上下文但不发送词表：

```text
Translate the comic dialogue into natural {{target_language}}.
Preserve tone and meaning; keep uncertain names in their original form for review.
{{visual}}
{{context}}
{{style}}
{{response_schema}}
```

翻译续卷时可加 `--context-from previous-book`，继承前卷已确认译名、待确认译名、最近对白和场景摘要。前卷的人工审校对白会重新计入上下文；两卷的目标语言须一致。续卷开始保存后，上下文作为快照固定；后续不传此选项即可按页继续。再次指定前卷时会检查来源和内容是否改变；如有变化会提示并继续使用原快照。如需使用更新后的前文，请创建新的续卷项目。

简中对白默认省略气泡末尾的普通句号；同泡内按口语语义使用逗号和必要句号，保留问号、感叹号及表达停顿、打断的省略号和破折号，连续话不因跨泡而额外加省略号。重复、强调、脏话力度和角色口吻也应保留。这里参考 [Netflix 简中字幕规范](https://studiopartner.netflix.net/studio/chinese-simplified-timed-text-style-guide) 的语气与连续性原则，但不采用其以空格替代逗号/句号、删重复词或字幕字数、行数、时长限制。模型请求带有简中风格指导，新生成对白入库时去除明确的泡末普通句号；原文以问号结尾、译文以“吗”结尾时补上问号。引号内标点、连续标点和人工审校文本保持原样。

`review` 导出独立的台词 JSON，包括页名、图片尺寸、区域 ID、OCR 原文、原文与气泡坐标和可编辑的 `translation`；有可用分析时还包含画格 ID、模型修正原文、说话人和场景摘要。这些上下文字段为只读参考，模型推断保留来源标记。每页的 `unresolved_regions` 列出未处理的文字候选、坐标和原因；`inspect` 也显示各页的候选数量，旧项目没有分析信息时显示 `null`。`apply-review` 校验原文、区域 ID 和导出时的 `original_translation` 后应用译文改动；同一句译文在审校文件和项目中都被改成不同时会提示冲突；未修改的审校行会保留项目中的新译文；几何、掩码和图片保持原样。之后运行 `export` 重新排版。继续翻译时，前文窗口使用审校后的台词。

版权页、制作名单等不需要翻译的内容，可在审校 JSON 的页面或区域上设置 `"preserve_original": true`，再用 `apply-review` 导入。翻译会跳过这些内容，擦字和排版会保留原图；设为 `false` 可恢复处理。

对已核对的未解决 OCR 区域，可使用 `camel repair-region book request.json` 重新生成安全区域并写入译文；请求包含 `page`、原页记录的 `source`、区域 ID（`region`）、`original_text`、需排除的误归属词序号（`exclude_words`）、`translation` 和可选的 `insert_before` / `insert_after` 阅读顺序锚点。涉及相邻区域内容拆分时还需提供 `split` 的原文、原译文及拆分后内容。命令校验源图与掩码，保留原始 OCR，更新检查点并使受影响场景摘要失效。审校 JSON 的区域可用 `font_size` 指定 10–64 像素字号，设为 `null` 恢复自动字号。 对确认误识别的已有区域，可使用 `camel remove-region book request.json`；请求包含页名 `page`、页面原图引用 `source`、区域 ID `region` 和识别原文 `original_text`。命令移除该区域并更新相关上下文、检查点和翻译计划，保留原始 OCR 供核查。


## 图像处理

检测器分别提供气泡、气泡内文字和画面文字候选。在原图中按连通笔画分行，气泡内相邻字母的共同高度用于识别正文行，避免周围装饰线和小标点压低行高。相邻行笔画相接导致漏读时，参考同泡文字高度重新识别。正文两侧成组的装饰线从识别图中排除，原始范围仍用于坐标和擦除。白色纸面上的彩色文字按图像对比度选择灰度转换，保留完整笔画。结合纸面连通性与连续笔画排除气泡外的细碎画线，保留同一纸面上的细字、标点和封闭字腔。根据封闭纸面与内部字形的包含关系剔除旁白框线，以正文行确定标点归属，避免小点和画线连带扩大识别范围；再将 CTC 词边界与字间空白对齐；较大的首字母按基线关联到对应文本行。跨行下沉的首字母完整归入首行，识别时按正文行高缩放，再把词坐标映回原图。同页相同来源的文本行只识别一次；相同文字与坐标的重复对白合并，同一基线上被完整读法覆盖的截断词去重，原始观察保存在 `analysis.initial_ocr`。所有对白共同参与画格排序和气泡分区，重叠文本框按逐词位置划分归属，每页复用画格与纸色候选检测结果。

从字框外缘估计背景色，对局部区域做连通分析和窄颈分离，限制与格间留白连通的气泡。物理纸面与相邻对白分割后的排版区分别处理，避免把字形内的小块白色当成整段对白的背景。字形掩码独立保存；平涂气泡使用纸色众数统一填色，独立渐变旁白在背景拟合通过后连续重建纸面。缺少完整纸面轮廓的气泡文字可使用窄掩码局部插值，避开相邻未识别笔画，并用周围纸面样本检验。无法形成有效排版区域的文字保留原图、结构化 OCR 和问题原因。

中文横排使用安全排版区每一行的可用区间，参考原文 OCR 字高统一页内字号，并围绕气泡主体平衡上下、左右留白。整段共用排版中轴，尾巴和与格间留白连通的开口分别参与空间约束；断行兼顾行宽、标点、孤字和字形外伸，同时保留语气停顿。中文使用 Jieba 分词辅助选择断行位置，并在本地优先保持已确认译名完整；空间不足时仍可在词内换行。内置词典来自 jieba-rs 0.11，许可证位于 `crates/camel/src/data/`。整页排版成功后执行擦字与绘制。字体可通过 `--font` 指定，默认优先寻找霞鹜文楷 Medium，再寻找本机中文字体。文楷可放在 `$XDG_DATA_HOME/camel/fonts/`（默认 `~/.local/share/camel/fonts/`）；字体独立于二进制加载。

## 库

工作区的 crate 都在 `crates/` 下：

- `crates/camel/`：核心库。`geometry::analyze` 将调用方提供的 `TextBlock` 转为带独立擦字掩码的 `Page`，`mask` 校验和修改掩码、填充与排版区，`translation::translate_page` 接受可替换的 `Translator`，`translate_missing_page` 用于补译，`layout::Typesetter` 负责字体测量与绘制，`comic::ComicSource` 从路径或任意可定位读取器读取漫画页。
- `crates/camel-vision/`：可共享的 `Engine`、独立检测器和行识别器。`Engine::load` 加载模型，`Engine::analyze` 接受 RGB 图像并返回 `Page` 与 `AnalysisMetadata`，结果可直接交给核心库擦字、翻译和排版。
- `crates/camel-project/`：项目文件格式与不涉及文件读写的操作，包括页面记录、原图校验、人工编辑、渲染决策和翻译计划修复；CLI 与工作台共用。
- `crates/camel-web/`：工作台使用的 WebAssembly 接口。
- `crates/camel-cli/`：命令行，负责文件读写和模型 API 调用。

```rust,no_run
use camel_vision::Engine;
use std::path::Path;

let engine = Engine::load(Path::new("models"), 2)?;
let image = image::open("page.jpg")?.to_rgb8();
let (page, analysis) = engine.analyze(&image)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## 开发

```sh
just check
just build
just dist
just web
```

`just web` 依次构建 WebAssembly（需要 `wasm32-unknown-unknown` 目标、与 `crates/camel-web` 中版本一致的 `wasm-bindgen` CLI 和 `wasm-opt`）、前端和前端测试。

全量识别和擦字评测直接读取本地图片，复用已加载的模型，擦字图在内存中校验：

```sh
just evaluate models/ pages/ results/ 2
```

末尾数字指定并发页数，默认 2。输出目录逐页保存原图路径、结构化结果和耗时，`summary.json` 汇总完成情况。图片和模型均独立于评测结果保存。

`release` 使用 thin LTO；`dist` 使用 fat LTO、单 codegen unit 和符号裁剪，图像处理保留优化级别 3。图片编解码只启用 PNG、JPEG，排版使用 ab_glyph，网络使用 ureq。

设计参考 [Koharu](https://github.com/koharu-rs/koharu) 的流水线及轮廓排版、[BallonsTranslator](https://github.com/dmMaze/BallonsTranslator) 的文字掩码分层、[MomoTranslator](https://github.com/alicewish/MomoTranslator) 的气泡分离，以及 [manga-image-translator](https://github.com/zyddnys/manga-image-translator) 的文本连通域与掩码修正流程。代码组织参考 cinekey、helianthus、tactus；Rust 风格参考 [Microsoft](https://microsoft.github.io/rust-guidelines/) 和 [Google](https://google.github.io/styleguide/rustguide.html)，命令行参考 [clig.dev](https://clig.dev/)。
