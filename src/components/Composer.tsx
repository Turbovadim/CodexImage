import { useRef, useState } from 'react'

export interface PendingAttachment {
  name: string
  data: string
  previewUrl: string
}

export interface ComposerTarget {
  nodeId: string
  prompt: string
  /** specific image being branched from, if picked in the lightbox */
  sourceImage?: string
  thumb?: string
}

const ASPECTS = ['auto', '1:1', '16:9', '9:16', '3:2']
const COUNTS = [1, 2, 3, 4]

function readAsBase64(file: File): Promise<string> {
  return new Promise(resolve => {
    const r = new FileReader()
    r.onload = () => resolve((r.result as string).split(',')[1])
    r.readAsDataURL(file)
  })
}

function Chip(props: { active?: boolean; onClick: () => void; children: React.ReactNode; title?: string }) {
  return (
    <button
      onClick={props.onClick}
      title={props.title}
      className={`rounded-full border px-2.5 py-1 text-[12px]
        ${props.active
          ? 'border-accent bg-accent/10 text-accent'
          : 'border-line text-dim hover:border-faint hover:text-ink'}`}
    >
      {props.children}
    </button>
  )
}

export function Composer(props: {
  target: ComposerTarget | null
  onClearTarget: () => void
  draft: string
  onDraftChange: (text: string) => void
  onSend: (text: string, options: { aspect: string; count: number }, attachments: PendingAttachment[]) => Promise<void>
}) {
  const { draft: text, onDraftChange: setText, target } = props
  const [aspect, setAspect] = useState('auto')
  const [count, setCount] = useState(1)
  const [attachments, setAttachments] = useState<PendingAttachment[]>([])
  const fileInputRef = useRef<HTMLInputElement>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  const addFiles = async (files: Iterable<File>) => {
    const added: PendingAttachment[] = []
    for (const file of files) {
      added.push({
        name: file.name || 'pasted.png',
        data: await readAsBase64(file),
        previewUrl: URL.createObjectURL(file),
      })
    }
    if (added.length) setAttachments(prev => [...prev, ...added])
  }

  const submit = async () => {
    const trimmed = text.trim()
    if (!trimmed) return
    setText('')
    const toSend = attachments
    setAttachments([])
    try {
      await props.onSend(trimmed, { aspect, count }, toSend)
    } catch (err) {
      alert((err as Error).message)
      setText(trimmed)
      setAttachments(toSend)
    }
  }

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-0 z-20 flex justify-center px-6 pb-5">
      <div className="pointer-events-auto w-full max-w-[720px] rounded-2xl border border-line bg-raised/95 px-4 pt-3 pb-3.5 shadow-[0_18px_60px_rgba(0,0,0,.55)] backdrop-blur-md">
        {target && (
          <div className="mb-2.5 flex items-center gap-2.5 rounded-xl border border-accent/30 bg-accent/8 px-2.5 py-1.5">
            {(target.sourceImage || target.thumb) && (
              <img
                src={target.sourceImage || target.thumb}
                alt=""
                className="h-9 w-9 rounded-lg border border-line object-cover"
              />
            )}
            <div className="min-w-0 flex-1">
              <div className="text-[11px] font-medium text-accent">
                Branching from{target.sourceImage ? ' this image' : ''}
              </div>
              <div className="truncate text-[11.5px] text-dim">{target.prompt}</div>
            </div>
            <button
              onClick={props.onClearTarget}
              title="Switch to starting a new root instead"
              className="shrink-0 rounded-md p-1 text-[13px] text-faint hover:text-ink"
            >
              ✕
            </button>
          </div>
        )}

        <div className="mb-2.5 flex flex-wrap items-center gap-3">
          <div className="flex gap-1.5" title="Aspect ratio">
            {ASPECTS.map(a => (
              <Chip key={a} active={aspect === a} onClick={() => setAspect(a)}>
                {a === 'auto' ? 'Auto' : a}
              </Chip>
            ))}
          </div>
          <div className="flex gap-1.5" title="Number of parallel variations (each becomes its own node)">
            {COUNTS.map(n => (
              <Chip key={n} active={count === n} onClick={() => setCount(n)}>×{n}</Chip>
            ))}
          </div>
          <Chip onClick={() => fileInputRef.current?.click()} title="Attach reference images">📎</Chip>
          <input
            ref={fileInputRef}
            type="file"
            accept="image/*"
            multiple
            hidden
            onChange={e => {
              if (e.target.files) void addFiles(e.target.files)
              e.target.value = ''
            }}
          />
        </div>

        {attachments.length > 0 && (
          <div className="mb-2.5 flex flex-wrap gap-2">
            {attachments.map((a, i) => (
              <div key={a.previewUrl} className="relative">
                <img src={a.previewUrl} alt={a.name} className="h-12 w-12 rounded-lg border border-line object-cover" />
                <button
                  onClick={() => setAttachments(prev => prev.filter((_, j) => j !== i))}
                  title="Remove"
                  className="absolute -top-1.5 -right-1.5 flex size-[18px] items-center justify-center rounded-full bg-danger text-[11px] text-white"
                >
                  ✕
                </button>
              </div>
            ))}
          </div>
        )}

        <div className="flex items-end gap-2.5">
          <textarea
            ref={textareaRef}
            value={text}
            rows={1}
            placeholder={target
              ? 'Describe the change or continuation… (Enter to generate)'
              : 'Describe the image you want… (Enter to generate, Shift+Enter for a new line)'}
            onChange={e => {
              setText(e.target.value)
              const el = textareaRef.current
              if (el) {
                el.style.height = 'auto'
                el.style.height = `${Math.min(el.scrollHeight, 160)}px`
              }
            }}
            onKeyDown={e => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault()
                void submit()
              }
            }}
            onPaste={e => {
              const files = [...e.clipboardData.items]
                .filter(i => i.type.startsWith('image/'))
                .map(i => i.getAsFile())
                .filter((f): f is File => f !== null)
              if (files.length) void addFiles(files)
            }}
            className="max-h-[160px] flex-1 resize-none rounded-xl border border-line bg-bg px-3.5 py-2.5
              text-[14px] text-ink outline-none placeholder:text-faint focus:border-accent"
          />
          <button
            onClick={() => void submit()}
            className="rounded-[10px] border border-accent-strong bg-accent-strong px-4.5 py-2.5 text-[13.5px] font-medium text-white hover:border-accent hover:bg-accent"
          >
            {target ? 'Branch' : 'Generate'}
          </button>
        </div>
      </div>
    </div>
  )
}
