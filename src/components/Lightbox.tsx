import { useEffect } from 'react'

function Action(props: {
  title: string
  children: React.ReactNode
  accent?: boolean
  onClick?: () => void
  href?: string
  download?: boolean
  newTab?: boolean
}) {
  const cls = `rounded-[10px] border px-3 py-2 text-[13px] shadow-[0_6px_24px_rgba(0,0,0,.45)] backdrop-blur-md
    ${props.accent
      ? 'border-accent-strong bg-accent-strong/90 font-medium text-white hover:bg-accent'
      : 'border-line bg-raised/80 text-ink hover:border-faint'}`
  if (props.href) {
    return (
      <a
        href={props.href}
        download={props.download}
        target={props.newTab ? '_blank' : undefined}
        rel="noopener"
        title={props.title}
        className={cls}
      >
        {props.children}
      </a>
    )
  }
  return <button onClick={props.onClick} title={props.title} className={cls}>{props.children}</button>
}

export function Lightbox(props: { src: string; onClose: () => void; onBranch?: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') props.onClose()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [props])

  return (
    <div onClick={props.onClose} className="fixed inset-0 z-50 bg-bg/97">
      <img src={props.src} alt="" className="h-full w-full object-contain" />
      <div
        onClick={e => e.stopPropagation()}
        className="absolute top-4 right-4 flex gap-2 opacity-90 transition-opacity hover:opacity-100"
      >
        {props.onBranch && (
          <Action accent title="Branch from this image" onClick={() => { props.onBranch!(); props.onClose() }}>
            ＋ Branch
          </Action>
        )}
        <Action title="Download" href={props.src} download>⬇</Action>
        <Action title="Open original" href={props.src} newTab>⤢</Action>
        <Action title="Close" onClick={props.onClose}>✕</Action>
      </div>
    </div>
  )
}
