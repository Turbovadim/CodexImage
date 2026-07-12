import assert from 'node:assert/strict'
import path from 'node:path'
import test from 'node:test'
import { isPathInside, manifestFilePath, parseOutputManifest } from './outputManifest.ts'

test('parses an ordered final selection without requiring a fixed output count', () => {
  assert.deepEqual(parseOutputManifest(JSON.stringify({
    summary: 'Created a three-page sequence.',
    complete: true,
    outputs: [
      { path: '/tmp/page-01.png', label: 'Page 1' },
      { path: '/tmp/page-02-v2.png', label: 'Page 2' },
      { path: '/tmp/page-03.png', label: 'Page 3' },
    ],
  })), {
    summary: 'Created a three-page sequence.',
    complete: true,
    outputs: [
      { path: '/tmp/page-01.png', label: 'Page 1' },
      { path: '/tmp/page-02-v2.png', label: 'Page 2' },
      { path: '/tmp/page-03.png', label: 'Page 3' },
    ],
  })
})

test('allows an incomplete refusal to return no final outputs', () => {
  assert.deepEqual(parseOutputManifest('{"summary":"The request could not be completed.","complete":false,"outputs":[]}'), {
    summary: 'The request could not be completed.',
    complete: false,
    outputs: [],
  })
})

test('rejects a complete manifest with no outputs', () => {
  assert.throws(
    () => parseOutputManifest('{"summary":"Done.","complete":true,"outputs":[]}'),
    /did not select any outputs/,
  )
})

test('rejects duplicate final paths instead of guessing which position is correct', () => {
  assert.throws(() => parseOutputManifest(JSON.stringify({
    summary: 'Bad manifest.',
    complete: true,
    outputs: [
      { path: '/tmp/page.png', label: 'Page 1' },
      { path: '/tmp/page.png', label: 'Page 2' },
    ],
  })), /selected more than once/)
})

test('rejects fields outside the final-output protocol', () => {
  assert.throws(() => parseOutputManifest(JSON.stringify({
    summary: 'Unexpected data.',
    complete: false,
    outputs: [],
    discarded: ['/tmp/attempt.png'],
  })), /did not match/)
  assert.throws(() => parseOutputManifest(JSON.stringify({
    summary: 'Unexpected data.',
    complete: true,
    outputs: [{ path: '/tmp/final.png', label: 'Final', confidence: 0.9 }],
  })), /missing its path or label/)
})

test('normalizes absolute paths and file URLs but rejects relative paths', () => {
  assert.equal(manifestFilePath('/tmp/image.png'), path.resolve('/tmp/image.png'))
  assert.equal(manifestFilePath('file:///tmp/image.png'), path.resolve('/tmp/image.png'))
  assert.throws(() => manifestFilePath('image.png'), /not absolute/)
})

test('checks generated paths without accepting siblings that share a prefix', () => {
  assert.equal(isPathInside('/tmp/generated/thread-a', '/tmp/generated/thread-a/image.png'), true)
  assert.equal(isPathInside('/tmp/generated/thread-a', '/tmp/generated/thread-ab/image.png'), false)
  assert.equal(isPathInside('/tmp/generated/thread-a', '/tmp/generated/thread-a'), false)
})
