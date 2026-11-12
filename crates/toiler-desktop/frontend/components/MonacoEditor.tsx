"use client";

import { useCallback, useEffect, useImperativeHandle, useMemo, useRef, type Ref, lazy, Suspense } from "react";
import { type EditorProps, type OnMount } from "@monaco-editor/react";
// 只导类型：import type 编译后整条擦除，不会在 Node 求值 monaco-editor 的
// ESM 入口——那个入口在模块顶层就碰 window，Next.js 的 SSR 预渲染会把
// "use client" 的模块也走一遍 import，值导入会直接让服务端崩溃。
import type * as monaco from "monaco-editor";

export interface MonacoEditorHandle {
  focus: () => void;
  getValue: () => string;
  getModel: () => monaco.editor.ITextModel | null;
  undo: () => void;
  redo: () => void;
}

interface MonacoFileEditorProps {
  value: string;
  language: string;
  dark: boolean;
  /**
   * 模型要带上真实文件路径：Monaco 的 TS worker 只按文件名后缀判定 ScriptKind，
   * 缺了 `.tsx`/`.jsx` 就会把 JSX 当普通 TS 解析，整片报语法错。
   */
  path?: string;
  onChange: (next: string) => void;
  onSave: (value: string) => void;
  onBlur?: () => void;
  /** 撤销栈变化：工具栏的撤销/恢复按钮靠它决定可不可点。 */
  onUndoStateChange?: (state: { canUndo: boolean; canRedo: boolean }) => void;
  handleRef?: Ref<MonacoEditorHandle>;
}

function mapLanguage(lang: string): string {
  const map: Record<string, string> = {
    javascript: "javascript",
    jsx: "javascript",
    typescript: "typescript",
    tsx: "typescript",
    python: "python",
    html: "html",
    css: "css",
    scss: "scss",
    less: "less",
    json: "json",
    jsonl: "json",
    markdown: "markdown",
    mdx: "markdown",
    yaml: "yaml",
    yml: "yaml",
    sql: "sql",
    rust: "rust",
    go: "go",
    java: "java",
    kotlin: "kotlin",
    swift: "swift",
    c: "c",
    cpp: "cpp",
    csharp: "csharp",
    h: "c",
    hpp: "cpp",
    php: "php",
    xml: "xml",
    bash: "bash",
    sh: "shell",
    zsh: "shell",
    fish: "shell",
    dockerfile: "dockerfile",
    env: "shell",
    gitignore: "shell",
  };
  return map[lang] ?? lang;
}

interface TypeScriptLanguageDefaults {
  setCompilerOptions: (options: Record<string, unknown>) => void;
  setDiagnosticsOptions: (options: Record<string, unknown>) => void;
}

interface TypeScriptNamespace {
  typescriptDefaults?: TypeScriptLanguageDefaults;
  javascriptDefaults?: TypeScriptLanguageDefaults;
}

/**
 * 0.57 起顶层是 `monaco.typescript`，旧的 `monaco.languages.typescript` 只剩一个
 * `{ deprecated: true }` 空壳；但不同构建入口下能拿到的那个不一样，两个都试。
 */
function resolveTypeScriptNamespace(monacoApi: unknown): TypeScriptNamespace | null {
  const api = monacoApi as {
    typescript?: TypeScriptNamespace;
    languages?: { typescript?: TypeScriptNamespace };
  } | null | undefined;
  return api?.typescript ?? api?.languages?.typescript ?? null;
}

/**
 * 这是文件编辑器，没有 tsconfig / node_modules / 项目文件图，TS 语言的语义校验
 * （Cannot find module、缺 JSX 命名空间等）几乎全是误报，所以只保留语法校验。
 * 但 compilerOptions 必须配好，尤其是 jsx：缺了它 .tsx 会一直报
 * "Cannot use JSX unless the '--jsx' flag is provided"。枚举用数值传入，避免依赖
 * 不同入口下可能缺失的 JsxEmit/ScriptTarget 导出名。
 */
function configureLanguageServices(monacoApi: unknown) {
  const ts = resolveTypeScriptNamespace(monacoApi);
  if (!ts) return;
  const compilerOptions = {
    jsx: 4, // JsxEmit.ReactJSX
    target: 99, // ScriptTarget.ESNext
    module: 99, // ModuleKind.ESNext
    moduleResolution: 2, // ModuleResolutionKind.NodeJs
    allowJs: true,
    allowNonTsExtensions: true,
    esModuleInterop: true,
    resolveJsonModule: true,
    isolatedModules: true,
    skipLibCheck: true,
  };
  const diagnosticsOptions = {
    noSemanticValidation: true,
    noSuggestionDiagnostics: true,
  };
  for (const defaults of [ts.typescriptDefaults, ts.javascriptDefaults]) {
    if (!defaults) continue;
    defaults.setCompilerOptions(compilerOptions);
    defaults.setDiagnosticsOptions(diagnosticsOptions);
  }
}

const LazyEditor = lazy(async (): Promise<{ default: React.ComponentType<EditorProps> }> => {
  const [{ default: loader }, monaco] = await Promise.all([
    import("@monaco-editor/loader"),
    import("monaco-editor"),
  ]);
  loader.config({ monaco: monaco as unknown as Parameters<typeof loader.config>[0]["monaco"] });
  configureLanguageServices(monaco);
  const mod = await import("@monaco-editor/react");
  const Editor = mod.Editor as unknown as React.ComponentType<EditorProps>;
  return { default: Editor };
});

export function MonacoFileEditor({
  value,
  language,
  dark,
  path,
  onChange,
  onSave,
  onUndoStateChange,
  handleRef,
}: MonacoFileEditorProps) {
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const undoStateListenerRef = useRef<(() => void) | null>(null);
  // 用 ref 转发，避免 handleMount （只该跑一次）因为回调身份变化而重建键盘监听。
  const onUndoStateChangeRef = useRef(onUndoStateChange);
  onUndoStateChangeRef.current = onUndoStateChange;

  // 撤销 / 恢复走模型自己的 undo 栈，而不走 editor.trigger("keyboard", "undo")。
  //
  // 这条命令在 Monaco 里是“模型优先、否则 DOM”的多实现命令（coreCommands.js 的
  // EditorOrNativeTextInputCommand）：优先给 `hasTextFocus()` 的编辑器，拿不到焦点
  // 就退成 `document.execCommand('undo')`。后者对编辑器内容完全无效，而
  // `hasTextFocus()` 靠 focus/blur 事件维护 —— WKWebView 里原生粘贴（Edit 菜单的
  // Cmd+V）会先从 textarea 收走焦点，粘贴完的焦点事件时序不保证把
  // `hasTextFocus` 恢复成 true。于是表现就是“粘贴之后 Cmd+Z 失灵，普通键入却
  // 没问题”。模型级 undo/redo 与命令内部真正做的事一模一样，但不会因为焦点
  // 状态掉进 DOM 分支。
  const undoRedoSilently = useCallback((isUndo: boolean) => {
    const model = editorRef.current?.getModel();
    if (!model) return;
    if (isUndo) model.undo();
    else model.redo();
  }, []);

  useImperativeHandle(handleRef, () => ({
    focus: () => editorRef.current?.focus(),
    getValue: () => editorRef.current?.getValue() ?? "",
    getModel: () => editorRef.current?.getModel() ?? null,
    undo: () => undoRedoSilently(true),
    redo: () => undoRedoSilently(false),
  }), [undoRedoSilently]);

  useEffect(() => () => {
    undoStateListenerRef.current?.();
    undoStateListenerRef.current = null;
  }, []);

  const monacoTheme = dark ? "vs-dark" : "light";

  const editorOptions = useMemo(() => {
    const fontSize = parseInt(getComputedStyle(document.documentElement).getPropertyValue("--code-font-size")) || 12;
    const fontFamily = getComputedStyle(document.documentElement).getPropertyValue("--font-mono");

    return {
      automaticLayout: true,
      minimap: { enabled: true },
      lineNumbers: "on" as const,
      glyphMargin: true,
      folding: true,
      wordWrap: "off" as const,
      scrollBeyondLastLine: false,
      tabSize: 4,
      insertSpaces: true,
      fontSize,
      fontFamily,
      theme: monacoTheme,
    };
  }, [monacoTheme]);

  // 用 @monaco-editor/react 实际交回的那个 monaco 实例来配置最可靠：
  // LazyEditor 初始化时拿到的模块命名空间未必就是编辑器用的同一份。
  const handleBeforeMount = useCallback<NonNullable<EditorProps["beforeMount"]>>((monacoApi) => {
    configureLanguageServices(monacoApi);
  }, []);

  const handleEditorChange = useCallback((next: string | undefined) => {
    if (typeof next !== "string") return;
    onChange(next);
  }, [onChange]);

  const handleExplicitSave = useCallback(() => {
    onSave(editorRef.current?.getValue() ?? value);
  }, [onSave, value]);

  const handleMount = useCallback<OnMount>((editor, monacoApi) => {
    editorRef.current = editor;
    // 只通过明确的保存命令写入文件，避免失焦或定时器提交瞬时空值。
    editor.addCommand(monacoApi.KeyMod.CtrlCmd | monacoApi.KeyCode.KeyS, handleExplicitSave);
    
    // Cmd+/ 或 Ctrl+/：注释/取消注释当前行
    editor.addCommand(
      monacoApi.KeyMod.CtrlCmd | monacoApi.KeyCode.Slash,
      () => editor.trigger('keyboard', 'editor.action.commentLine', {})
    );
    
    // Alt+F：格式化文档
    editor.addCommand(
      monacoApi.Alt | monacoApi.KeyCode.F,
      () => editor.trigger('keyboard', 'editor.action.formatDocument', {})
    );
    
    // Cmd+D：选中下一个相同单词
    editor.addCommand(
      monacoApi.KeyMod.CtrlCmd | monacoApi.KeyCode.KeyD,
      () => editor.trigger('keyboard', 'actions.selection.match.findNextMatch', {})
    );
    
    // Ctrl+空格：触发建议/智能感知
    editor.addCommand(
      monacoApi.KeyMod.CtrlCmd | monacoApi.KeyCode.Space,
      () => editor.trigger('keyboard', 'editor.action.triggerSuggest', {})
    );

    // 撤销 / 恢复：不能只靠 Monaco 自带的绑定。Tauri 的 WebView（WKWebView /
    // WebView2）会先把 Ctrl/Cmd+Z 当成原生编辑命令吃掉，keydown 到不了编辑器，
    // 表现就是「撤销没反应」。所以在 document 的 capture 阶段先擒下来，自己
    // 触发 Monaco 的 undo 栈；stopPropagation 是必需的，否则 Monaco 的默认绑定
    // 会再撤一步。
    const handleUndoRedoKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
      const key = event.key.toLowerCase();
      const isUndo = key === "z" && !event.shiftKey;
      // Ctrl+U 是常见的「恢复」习惯键，与 Ctrl+Y / Cmd+Shift+Z 等价。
      const isRedo = key === "y" || key === "u" || (key === "z" && event.shiftKey);
      if (!isUndo && !isRedo) return;
      const dom = editor.getDomNode();
      if (!dom || !(event.target instanceof Node) || !dom.contains(event.target)) return;
      event.preventDefault();
      event.stopPropagation();
      undoRedoSilently(isUndo);
    };
    document.addEventListener("keydown", handleUndoRedoKey, true);

    // 粘贴单独成一步：WebKit 的原生粘贴可能和前面的键入落到同一个 undo 元素，
    // 一次 Cmd+Z 就只撤掉一半（或看起来「粘贴的内容撤不掉」）。在 Monaco 处理
    // 粘贴之前先断开 undo 分组，粘贴就会是独立的可撤销单元。
    const handlePasteBoundary = (event: Event) => {
      const dom = editor.getDomNode();
      if (!dom || !(event.target instanceof Node) || !dom.contains(event.target)) return;
      editor.pushUndoStop();
    };
    document.addEventListener("paste", handlePasteBoundary, true);

    undoStateListenerRef.current?.();
    undoStateListenerRef.current = () => {
      document.removeEventListener("keydown", handleUndoRedoKey, true);
      document.removeEventListener("paste", handlePasteBoundary, true);
    };

    // 工具栏按钮的可用状态：撤销本身也会改内容，所以同一个回调同时负责两者。
    const notifyUndoState = () => {
      const model = editor.getModel();
      onUndoStateChangeRef.current?.({
        canUndo: model?.canUndo() ?? false,
        canRedo: model?.canRedo() ?? false,
      });
    };
    editor.onDidChangeModelContent(notifyUndoState);
    editor.onDidChangeModel(notifyUndoState);
    notifyUndoState();
  }, [handleExplicitSave, undoRedoSilently]);

  return (
    <Suspense fallback={<div style={{ padding: 24, color: "var(--text-muted)", fontSize: 12 }}>Loading editor...</div>}>
      <LazyEditor
        defaultValue={value}
        language={mapLanguage(language)}
        path={path}
        beforeMount={handleBeforeMount}
        onChange={handleEditorChange}
        onMount={handleMount}
        options={editorOptions}
      />
    </Suspense>
  );
}