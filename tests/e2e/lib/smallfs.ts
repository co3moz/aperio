import { execFile } from 'node:child_process'
import { mkdtemp, open, rm, statfs } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { promisify } from 'node:util'

const run = promisify(execFile)

/**
 * A small filesystem that can actually be filled, for the one failure that
 * cannot be faked (`planned_features.md` #100).
 *
 * Every portable way to simulate a full disk is a different failure wearing
 * its clothes. A read-only directory refuses new files but not writes to the
 * ones SQLite already holds open, which is the opposite of the case. Filling
 * the real temp directory takes the machine with it. Both would let a test
 * pass while asserting its property on a path where nothing went wrong.
 *
 * So this makes a genuine filesystem of a few megabytes and genuinely fills
 * it, by the mechanism each platform actually has:
 *
 * - **macOS**: `hdiutil`, which creates and attaches a disk image without
 *   root, so this runs on a developer's machine as it stands.
 * - **Linux**: a file, an ext4 on it, mounted through a loop device, which
 *   needs root. CI runners have passwordless sudo; a machine that does not is
 *   reported unsupported rather than approximated.
 *
 * Anywhere else, `supported` is false and the caller skips.
 */
export interface SmallFs {
  /** The mounted directory. */
  dir: string
  /** Fills every remaining byte. Returns how many bytes it managed to write. */
  fill(): Promise<number>
  /** Frees what `fill` wrote, so cleanup and teardown can do their work. */
  free(): Promise<void>
  /** Unmounts and removes everything. */
  cleanup(): Promise<void>
}

/** Why this platform cannot host one, or null when it can. */
export async function smallFsUnsupported(): Promise<string | null> {
  if (process.platform === 'darwin') return null
  if (process.platform !== 'linux') {
    return `a real full filesystem needs hdiutil or a loop mount; ${process.platform} has neither`
  }
  try {
    // -n: never prompt. A machine that would ask for a password is a machine
    // this cannot run on unattended.
    await run('sudo', ['-n', 'true'])
  } catch {
    return 'mounting a loop device needs passwordless sudo'
  }
  for (const tool of ['mkfs.ext4', 'losetup']) {
    try {
      await run('which', [tool])
    } catch {
      return `${tool} is not installed`
    }
  }
  return null
}

/** Creates and mounts one. Call `smallFsUnsupported` first. */
export async function smallFs(megabytes = 12): Promise<SmallFs> {
  const work = await mkdtemp(join(tmpdir(), 'aperio-smallfs-'))
  const image = join(work, 'image')
  const filler = () => join(mountedDir, 'filler.bin')
  let mountedDir = ''
  let detach: () => Promise<void>

  if (process.platform === 'darwin') {
    // A volume name that is its own directory under /Volumes, unique so two
    // concurrent runs cannot collide on it.
    const volume = `aperio-${process.pid}-${Date.now()}`
    await run('hdiutil', [
      'create',
      '-size',
      `${megabytes}m`,
      '-fs',
      'HFS+',
      '-volname',
      volume,
      '-quiet',
      `${image}.dmg`,
    ])
    const { stdout } = await run('hdiutil', ['attach', `${image}.dmg`, '-nobrowse'])
    // "…/disk4s1        Apple_HFS      /Volumes/aperio-123"
    const line = stdout.trim().split('\n').at(-1) ?? ''
    const device = line.split(/\s+/)[0]
    mountedDir = line.slice(line.indexOf('/Volumes'))
    if (!mountedDir) throw new Error(`hdiutil attach said: ${stdout}`)
    detach = async () => {
      await run('hdiutil', ['detach', device, '-force']).catch(() => undefined)
    }
  } else {
    mountedDir = join(work, 'mnt')
    await run('mkdir', ['-p', mountedDir])
    await run('truncate', ['-s', `${megabytes}M`, image])
    await run('mkfs.ext4', ['-q', '-F', image])
    await run('sudo', ['mount', '-o', 'loop', image, mountedDir])
    // Mounted by root, written by us.
    await run('sudo', ['chown', `${process.getuid?.()}:${process.getgid?.()}`, mountedDir])
    detach = async () => {
      await run('sudo', ['umount', mountedDir]).catch(() => undefined)
    }
  }

  return {
    dir: mountedDir,
    async fill(): Promise<number> {
      // Written in shrinking pieces rather than one big one: the last bytes of
      // a filesystem are not claimable in a single large write, and "nearly
      // full" is not the case under test.
      const handle = await open(filler(), 'w')
      let written = 0
      try {
        for (const size of [1 << 20, 1 << 16, 1 << 12, 512, 64]) {
          const block = Buffer.alloc(size, 0x61)
          for (;;) {
            try {
              await handle.write(block)
              written += size
            } catch {
              break
            }
          }
        }
        await handle.sync().catch(() => undefined)
      } finally {
        await handle.close()
      }
      return written
    },
    async free(): Promise<void> {
      await rm(filler(), { force: true })
    },
    async cleanup(): Promise<void> {
      await rm(filler(), { force: true }).catch(() => undefined)
      await detach()
      await rm(work, { recursive: true, force: true }).catch(() => undefined)
    },
  }
}

/** Bytes still free on the filesystem holding `dir`. */
export async function freeBytes(dir: string): Promise<number> {
  const s = await statfs(dir)
  return Number(s.bsize) * Number(s.bavail)
}
