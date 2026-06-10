# Issue: 点击好友家园留言板 → touchHLE 整机崩溃(NSDateFormatter 小写 hh)

- **状态**:已修复(`src/frameworks/foundation/ns_date_formatter.rs`,2026-06-10)
- **优先级**:高(确定性崩溃,只要留言列表≥1条必崩)
- **影响**:touchHLE(Mac/iOS 模拟器);真机原版不受影响(NSDateFormatter 由系统实现)
- **逆向依据**:c9.i64,全部 Thumb 地址已验证

## 现象

在 touchHLE 里拜访好友庄园后,点界面上的"信件/留言"按钮进入留言板,**整个模拟器立即崩溃**(Rust panic → abort)。表现为"刚拜访能看到家园,一进留言板就崩"。空留言列表不崩(不渲染 cell)。

## 根因(确定性)

调用链:
```
-[UserInfoLayer onButtonLettersSelected:] @0x5b890
  → -[NetworkManager getMessageFromServer] @0xea2a8   (发命令 1029 拉留言列表)
  → 服务器回 1029 → 填 MessageViewController._displayList
  → tableView 每行 → -[MessageViewController configureCell:forIndexPath:] @0x1a7718
```

`configureCell` 在 `0x1a7eb6–0x1a7f92` 把每条留言的 `sendTime` 格式化成日期串:

```objc
v82 = [NSDate dateWithTimeIntervalSince1970:(double)[msg sendTime]];   // 0x1a7ed4
v84 = [[NSDateFormatter alloc] init];                                  // 0x1a7eee
[v84 setAMSymbol:@"AM"]; [v84 setPMSymbol:@"PM"];                      // touchHLE 未实现→no-op,无害
[v84 setDateFormat:@"MM/dd/yyyy hh:mm:ss"];                            // 0x1a7f54  ★小写 hh
v85 = [v84 stringFromDate:v82];                                       // 0x1a7f74  ← 崩这
```

touchHLE 的 `-[NSDateFormatter stringFromDate:]`(`ns_date_formatter.rs`)旧实现只替换
`yyyy/YYYY/MM/dd/HH/mm/ss`,**不替换小写 `hh`(12 小时制)**。替换后串里残留 `hh`,函数末尾
有一段开发期断言:扫到任意残留 `A-Za-z` 字母即 `unimplemented!("...unsubstituted format pattern: h")`
→ **Rust panic → touchHLE abort**。

格式串 `"MM/dd/yyyy hh:mm:ss"` 的小写 `hh` 必然残留,故只要留言列表渲染任一条带 `sendTime` 的 cell 就必崩。

> 这也是"发留言板信息给好友不工作"的真相:留言其实发出去了(服务端 1028 已记录、1029 能返回),
> 但收方一打开留言板查看就崩,看起来像"没成功"。

## 修复

`src/frameworks/foundation/ns_date_formatter.rs` 的 `stringFromDate:`:

1. 补 12 小时制:`hh`/`h` → `hour % 12`(0 点算 12),放在 `HH` 之后。
2. 补 AM/PM:`a` → `"AM"/"PM"`,放在数字字段替换之后(避免插入的字母被再次当模式)。
3. `setDateFormat:` 没调过(`date_format=None`)时安全降级返回空串,不再 `.unwrap()` panic。
4. **删掉"剩余字母即 `unimplemented!()`"那段开发期断言**:它把"遇到未支持/纯装饰的格式模式"
   升级成整机 abort。改为直接输出已替换结果,未支持模式以字面字母呈现(纯外观,不崩)。

## 教训

touchHLE 里残留的 `unimplemented!()`/`.unwrap()` 这类开发期断言,在跑真实大型 App 时会把
"某个边角格式/选择子没支持"放大成整机崩溃。渲染/格式化这种每帧/每 cell 跑的热路径上的断言,
应一律降级为日志 + 合理兜底,绝不 panic。
