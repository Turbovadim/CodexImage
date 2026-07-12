import path from 'node:path'
import { fileURLToPath } from 'node:url'

export interface FinalOutput {
  /** Exact absolute path returned by Codex's image-generation tool. */
  path: string
  /** Short semantic label such as "Page 3" or "Blue variation". */
  label: string
}

export interface OutputManifest {
  summary: string
  /** Whether the selected outputs fully satisfy the original request. */
  complete: boolean
  /** Final deliverables in the order intended by Codex. */
  outputs: FinalOutput[]
}

/**
 * Passed to `codex exec --output-schema`. It deliberately constrains only the
 * final selection, not the number of generations Codex may make while
 * correcting its own work.
 */
export const OUTPUT_MANIFEST_SCHEMA = {
  type: 'object',
  properties: {
    summary: {
      type: 'string',
      maxLength: 500,
      description: 'One concise sentence describing the final deliverables or why none could be produced.',
    },
    complete: {
      type: 'boolean',
      description: 'True only when the selected outputs fully satisfy every deliverable in the request.',
    },
    outputs: {
      type: 'array',
      description: 'Only final deliverables, in semantic order. Superseded attempts must be omitted.',
      items: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            minLength: 1,
            description: 'Exact absolute saved path returned by the image-generation tool.',
          },
          label: {
            type: 'string',
            minLength: 1,
            maxLength: 120,
            description: 'Short human-readable name that identifies this output in the ordered set.',
          },
        },
        required: ['path', 'label'],
        additionalProperties: false,
      },
    },
  },
  required: ['summary', 'complete', 'outputs'],
  additionalProperties: false,
} as const

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function hasOnlyKeys(value: Record<string, unknown>, allowed: readonly string[]): boolean {
  const keys = Object.keys(value)
  return keys.length === allowed.length && keys.every(key => allowed.includes(key))
}

export function parseOutputManifest(text: string): OutputManifest {
  let value: unknown
  try {
    value = JSON.parse(text)
  } catch {
    throw new Error('the final response was not valid JSON')
  }
  if (!isRecord(value)
    || !hasOnlyKeys(value, ['summary', 'complete', 'outputs'])
    || typeof value.summary !== 'string'
    || typeof value.complete !== 'boolean'
    || !Array.isArray(value.outputs)) {
    throw new Error('the final response did not match the output manifest')
  }

  const summary = value.summary.trim()
  if (summary.length > 500) throw new Error('the final summary was too long')

  const outputs: FinalOutput[] = []
  const paths = new Set<string>()
  for (const raw of value.outputs) {
    if (!isRecord(raw)
      || !hasOnlyKeys(raw, ['path', 'label'])
      || typeof raw.path !== 'string'
      || typeof raw.label !== 'string') {
      throw new Error('an output was missing its path or label')
    }
    const outputPath = raw.path.trim()
    const label = raw.label.trim()
    if (!outputPath || !label) throw new Error('an output had an empty path or label')
    if (label.length > 120) throw new Error('an output label was too long')
    if (paths.has(outputPath)) throw new Error('the same generated image was selected more than once')
    paths.add(outputPath)
    outputs.push({ path: outputPath, label })
  }
  if (value.complete && outputs.length === 0) {
    throw new Error('a complete manifest did not select any outputs')
  }
  return { summary, complete: value.complete, outputs }
}

/** Accept absolute paths and file URLs, but never guess a relative location. */
export function manifestFilePath(value: string): string {
  let candidate = value
  if (candidate.startsWith('file:')) {
    try {
      candidate = fileURLToPath(candidate)
    } catch {
      throw new Error('a selected image used an invalid file URL')
    }
  }
  if (!path.isAbsolute(candidate)) throw new Error('a selected image path was not absolute')
  return path.resolve(candidate)
}

/** True only for a descendant of root; the root directory itself is not a file. */
export function isPathInside(root: string, candidate: string): boolean {
  const relative = path.relative(path.resolve(root), path.resolve(candidate))
  return relative !== '' && !relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative)
}
