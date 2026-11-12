/**
 * Java 包名折叠。
 *
 * Java 的包在磁盘上就是嵌套目录（`com/example/demo`），逐级展开时一个
 * `com.example.demo` 就要占三行。IDEA 一类 IDE 的做法是把「只含一个子目录、
 * 自身没有文件」的每一级合并成一行，显示成 `com.example.demo`。
 *
 * 这里只对 `<...>/src/main/java` 与 `<...>/src/test/java` 两棵树生效（用户
 * 选定的范围）：其它目录里的单子目录链保持原样，避免把 `app/api/agent/[id]`
 * 这种路径压成一行。
 *
 * 纯函数 + 注入目录读取，便于单测；路由侧传入 `fs.readdirSync` 的实现。
 */

/** 目录列表里的一项，只保留链式合并需要的字段。 */
export interface JavaPackageChild {
  name: string;
  isDir: boolean;
}

/** 一条被折叠的包链：展示名 + 最内层目录。 */
export interface JavaPackageChain {
  /** 点号分隔的展示名，例如 `com.example.demo`。 */
  name: string;
  /** 最内层目录的绝对路径，客户端用它打开/引用这个包。 */
  packagePath: string;
  /** 构成这条链的各级目录名。长度 1 表示没有可折叠的层级。 */
  segments: string[];
}

/** 链式合并的最大深度。正常包名远小于这个值，用来给异常目录树兜底。 */
export const JAVA_PACKAGE_CHAIN_MAX_DEPTH = 16;

/** 源码根的第一级与第三级：`src/<main|test>/java`。 */
const SOURCE_ROOT_PREFIX = "src";
const SOURCE_ROOT_SUFFIX = "java";
/** 只认 Maven/Gradle 最常见的两组 sourceSet（用户选定的范围）。 */
const SOURCE_SETS = new Set(["main", "test"]);

function splitPath(filePath: string): string[] {
  return filePath.split(/[\\/]+/).filter(Boolean);
}

/**
 * 路径是否位于 `src/main/java` 或 `src/test/java`（含该目录本身）。
 *
 * 只看路径段，不做磁盘探测：只要目录树里出现这个三元组就按 Java 源码根处理。
 */
export function isInsideJavaSourceRoot(filePath: string): boolean {
  const segments = splitPath(filePath);
  for (let index = 0; index + 2 < segments.length; index += 1) {
    if (segments[index] !== SOURCE_ROOT_PREFIX) continue;
    if (!SOURCE_SETS.has(segments[index + 1])) continue;
    if (segments[index + 2] !== SOURCE_ROOT_SUFFIX) continue;
    return true;
  }
  return false;
}

const JAVA_IDENTIFIER = /^[\p{L}_$][\p{L}\p{N}_$]*$/u;

/**
 * 是否是合法的 Java 包名段。
 *
 * 用 Java 标识符规则而不是「全小写」：反常规但合法的 `Demo` / `E2E` 目录仍然
 * 应该折叠，而 `META-INF`、`resources`、`foo.bar` 这类明显不是包名的目录不折叠。
 */
export function isJavaPackageSegment(name: string): boolean {
  return JAVA_IDENTIFIER.test(name);
}

/**
 * 从 `parentDir/childName` 开始向下合并包链。
 *
 * 只有当当前目录「有且仅有一个子目录、且自身没有文件」时才继续把下一级并入
 * 展示名 —— 有类文件的那一级属于终点包，必须留在可展开的行上。
 */
export function mergeJavaPackageChain(
  parentDir: string,
  childName: string,
  readChildren: (dir: string) => JavaPackageChild[],
  options: {
    joinPath?: (dir: string, name: string) => string;
    maxDepth?: number;
  } = {},
): JavaPackageChain {
  const joinPath = options.joinPath ?? ((dir, name) => `${dir}/${name}`);
  const maxDepth = options.maxDepth ?? JAVA_PACKAGE_CHAIN_MAX_DEPTH;

  const segments = [childName];
  let packagePath = joinPath(parentDir, childName);

  while (segments.length < maxDepth) {
    const children = readChildren(packagePath);
    if (children.length !== 1) break;
    const [next] = children;
    if (!next.isDir || !isJavaPackageSegment(next.name)) break;
    segments.push(next.name);
    packagePath = joinPath(packagePath, next.name);
  }

  return { name: segments.join("."), packagePath, segments };
}
