import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"

export interface ThreadRef { environmentId: string; threadId: string }

/** A thread id is only unique within its environment. */
export function threadRef(value: unknown): ThreadRef | null {
  if (!value || typeof value !== "object") return null
  const { environmentId, threadId } = value as Partial<ThreadRef>
  if (typeof environmentId !== "string" || !/^[\w-]{1,128}$/.test(environmentId) ||
      typeof threadId !== "string" || !threadId.trim() || threadId.length > 1024 || /[\x00-\x1f\x7f]/.test(threadId)) return null
  return { environmentId, threadId }
}

export const threadKey = (ref: ThreadRef): string => JSON.stringify([ref.environmentId, ref.threadId])

/** This installation's environment id, kept beside the database T3 writes. */
export function environmentId(dbPath: string): string | null {
  try {
    const id = readFileSync(join(dirname(dbPath), "environment-id"), "utf8").trim()
    return /^[\w-]{1,128}$/.test(id) ? id : null
  } catch {
    return null
  }
}
