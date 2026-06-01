// ===== 0x0000ef40  -[iMoleVillageAppDelegate applicationDidFinishLaunching:] =====
void __cdecl -[iMoleVillageAppDelegate applicationDidFinishLaunching:](iMoleVillageAppDelegate *self, SEL a2, id a3)
{
  __int64 v3; // d8
  __int64 v4; // d9
  __int64 v5; // d10
  __int64 v6; // d11
  __int64 v7; // d12
  __int64 v8; // d13
  __int64 v9; // d14
  __int64 v10; // d15
  UIDevice *v12; // r0
  NSString *v13; // r0
  UIDevice *v14; // r0
  id v15; // r0
  NSUserDefaults *v16; // r0
  UIDevice *v17; // r0
  NSString *v18; // r0
  NSString *v19; // r4
  UIDevice *v20; // r0
  id v21; // r9
  NSString *v22; // r0
  NSURL *v23; // r0
  NSMutableURLRequest *v24; // r0
  UIApplication *v25; // r0
  NetworkManager *v26; // r0
  iRate *v27; // r0
  GameSettings *v28; // r0
  GameData *v29; // r0
  GameData *v30; // r0
  UserInfoData *v31; // r0
  id v32; // r0
  UIWindow *v33; // r4
  UIScreen *v34; // r1
  __int64 v35; // r2
  __int64 v36; // kr00_8
  id v37; // r8
  RootViewController *v38; // r0
  RootViewController *v39; // r0
  UIWindow *window; // r1
  __int64 v41; // r2
  __int64 v42; // kr08_8
  EAGLView *v43; // r4
  unsigned __int8 v44; // r5
  id v45; // r0
  UIWindow *v46; // r4
  id v47; // r0
  id v48; // r4
  UIApplication *v49; // r0
  time_t v50; // r4
  int v51; // r0
  id v52; // r0
  id v53; // r4
  id v54; // r5
  id v55; // r0
  UIDevice *v56; // r0
  NSString *v57; // r0
  AdWallsManager *v58; // r0
  id v59; // r0
  __int64 v60; // [sp+5Ch] [bp-ACh] BYREF
  __int64 v61; // [sp+64h] [bp-A4h]
  __int64 v62; // [sp+6Ch] [bp-9Ch] BYREF
  __int64 v63; // [sp+74h] [bp-94h]
  __int64 v64; // [sp+B0h] [bp-58h]
  __int64 v65; // [sp+B8h] [bp-50h]
  __int64 v66; // [sp+C0h] [bp-48h]
  __int64 v67; // [sp+C8h] [bp-40h]
  __int64 v68; // [sp+D0h] [bp-38h]
  __int64 v69; // [sp+D8h] [bp-30h]
  __int64 v70; // [sp+E0h] [bp-28h]
  __int64 v71; // [sp+E8h] [bp-20h]

  v64 = v3;
  v65 = v4;
  v66 = v5;
  v67 = v6;
  v68 = v7;
  v69 = v8;
  v70 = v9;
  v71 = v10;
  +[NewRelicAgent startWithApplicationToken:](
    &OBJC_CLASS___NewRelicAgent,
    "startWithApplicationToken:",
    CFSTR("AA29f6fa42c981f747eaa2d4d4158081fc0ca0c746"));
  -[iMoleVillageAppDelegate umengAnalyze](self, "umengAnalyze");
  -[iMoleVillageAppDelegate umengTrack](self, "umengTrack");
  v12 = +[UIDevice currentDevice](&OBJC_CLASS___UIDevice, "currentDevice");
  v13 = -[UIDevice systemVersion](v12, "systemVersion");
  if ( COERCE_FLOAT(-[NSString floatValue](v13, "floatValue")) >= 3.2 )
    +[CrashLog initCrashLogNotShowViewWithDelegate:andGameType:](
      &OBJC_CLASS___CrashLog,
      "initCrashLogNotShowViewWithDelegate:andGameType:",
      self,
      CFSTR("imole"));
  +[IMAdTracker initWithAppID:](
    &OBJC_CLASS___IMAdTracker,
    "initWithAppID:",
    CFSTR("54d1f306-f248-4faf-bf6e-79d240c988a2"));
  +[IMAdTracker reportAppDownloadGoal](&OBJC_CLASS___IMAdTracker, "reportAppDownloadGoal");
  v14 = +[UIDevice currentDevice](&OBJC_CLASS___UIDevice, "currentDevice");
  v15 = -[UIDevice hasIllegalApp](v14, "hasIllegalApp");
  if ( objc_msgSend(v15, "intValue") )
    goto LABEL_7;
  v16 = +[NSUserDefaults standardUserDefaults](&OBJC_CLASS___NSUserDefaults, "standardUserDefaults");
  if ( !-[NSUserDefaults boolForKey:](v16, "boolForKey:", CFSTR("activation")) )
  {
    v17 = +[UIDevice currentDevice](&OBJC_CLASS___UIDevice, "currentDevice");
    v18 = -[UIDevice name](v17, "name");
    v19 = -[NSString stringByAddingPercentEscapesUsingEncoding:](v18, "stringByAddingPercentEscapesUsingEncoding:", 4);
    v20 = +[UIDevice currentDevice](&OBJC_CLASS___UIDevice, "currentDevice");
    v21 = -[UIDevice taomeeUDID](v20, "taomeeUDID");
    v22 = +[NSString stringWithFormat:](
            &OBJC_CLASS___NSString,
            "stringWithFormat:",
            CFSTR("http://c.appcpa.co/e?appkey=%@&deviceName=%@&uid=%@"),
            CFSTR("ce3914ae6cb2468997fa4ddbeabd5879"),
            v19,
            v21);
    v23 = +[NSURL URLWithString:](&OBJC_CLASS___NSURL, "URLWithString:", v22);
    v24 = +[NSMutableURLRequest requestWithURL:](&OBJC_CLASS___NSMutableURLRequest, "requestWithURL:", v23);
    +[NSURLConnection connectionWithRequest:delegate:](
      &OBJC_CLASS___NSURLConnection,
      "connectionWithRequest:delegate:",
      v24,
      self);
  }
  +[TalkingDataGA onStart:withChannelId:](
    &OBJC_CLASS___TalkingDataGA,
    "onStart:withChannelId:",
    CFSTR("6ED592596D456246F1E8E629BCB7F52B"),
    &stru_B0DA58);
  v25 = +[UIApplication sharedApplication](&OBJC_CLASS___UIApplication, "sharedApplication");
  -[UIApplication setStatusBarHidden:](v25, "setStatusBarHidden:", 1);
  v26 = +[NetworkManager sharedInstance](&OBJC_CLASS___NetworkManager, "sharedInstance");
  -[NetworkManager checkNetWorking](v26, "checkNetWorking");
  v27 = +[iRate sharedInstance](&OBJC_CLASS___iRate, "sharedInstance");
  -[iRate setDelegate:](v27, "setDelegate:", self);
  v28 = +[GameSettings sharedInstance](&OBJC_CLASS___GameSettings, "sharedInstance");
  -[GameSettings loadSettings](v28, "loadSettings");
  v29 = +[GameData sharedInstance](&OBJC_CLASS___GameData, "sharedInstance");
  -[GameData loadUserPurchaseInfo](v29, "loadUserPurchaseInfo");
  v30 = +[GameData sharedInstance](&OBJC_CLASS___GameData, "sharedInstance");
  v31 = -[GameData userInfoData](v30, "userInfoData");
  v32 = -[UserInfoData vipGoldWithNewType](v31, "vipGoldWithNewType");
  if ( (int)objc_msgSend(v32, "intValue") >= 1 )
  {
LABEL_7:
    -[iMoleVillageAppDelegate showCheatWarningMessage](self, "showCheatWarningMessage");
  }
  else
  {
    v33 = +[UIWindow alloc](&OBJC_CLASS___UIWindow, "alloc");
    v34 = +[UIScreen mainScreen](&OBJC_CLASS___UIScreen, "mainScreen");
    if ( v34 )
    {
      objc_msgSend_stret(&v62, (SEL)v34, "bounds");
      v35 = v62;
      v36 = v63;
    }
    else
    {
      v62 = 0;
      v63 = 0;
      v35 = 0;
      v36 = 0;
    }
    self->window = -[UIWindow initWithFrame:](v33, "initWithFrame:", v35, v36);
    if ( !+[CCDirector setDirectorType:](&OBJC_CLASS___CCDirector, "setDirectorType:", 3) )
      +[CCDirector setDirectorType:](&OBJC_CLASS___CCDirector, "setDirectorType:", 0);
    v37 = +[CCDirector sharedDirector](&OBJC_CLASS___CCDirector, "sharedDirector");
    objc_msgSend(v37, "setRunLoopCommon:", 1);
    v38 = +[RootViewController alloc](&OBJC_CLASS___RootViewController, "alloc");
    v39 = -[RootViewController initWithNibName:bundle:](v38, "initWithNibName:bundle:", 0, 0);
    self->viewController = v39;
    -[RootViewController setWantsFullScreenLayout:](v39, "setWantsFullScreenLayout:", 1);
    window = self->window;
    if ( window )
    {
      objc_msgSend_stret(&v60, (SEL)window, "bounds");
      v41 = v60;
      v42 = v61;
    }
    else
    {
      v60 = 0;
      v61 = 0;
      v41 = 0;
      v42 = 0;
    }
    v43 = +[EAGLView viewWithFrame:pixelFormat:depthFormat:preserveBackbuffer:sharegroup:multiSampling:numberOfSamples:](
            &OBJC_CLASS___EAGLView,
            "viewWithFrame:pixelFormat:depthFormat:preserveBackbuffer:sharegroup:multiSampling:numberOfSamples:",
            v41,
            v42,
            kEAGLColorFormatRGB565,
            0,
            1,
            0,
            0,
            0);
    objc_msgSend(v37, "setOpenGLView:", v43);
    -[EAGLView setMultipleTouchEnabled:](v43, "setMultipleTouchEnabled:", 1);
    v44 = (unsigned __int8)objc_msgSend(v37, "enableRetinaDisplay:", 1);
    v45 = +[TMDevice sharedDevice](&OBJC_CLASS___TMDevice, "sharedDevice");
    if ( v44 )
      objc_msgSend(v45, "setIsRetinaDisplay:", 1);
    else
      objc_msgSend(v45, "setIsRetinaDisplay:", 0);
    objc_msgSend(v37, "setDeviceOrientation:", 1);
    objc_msgSend(v37, "setAnimationInterval:", 286331153, 1066471697);
    -[EAGLView setMultipleTouchEnabled:](v43, "setMultipleTouchEnabled:", 1);
    -[RootViewController setView:](self->viewController, "setView:", v43);
    v46 = self->window;
    v47 = -[RootViewController view](self->viewController, "view");
    -[UIWindow addSubview:](v46, "addSubview:", v47);
    -[UIWindow setRootViewController:](self->window, "setRootViewController:", self->viewController);
    -[UIWindow makeKeyAndVisible](self->window, "makeKeyAndVisible");
    v48 = +[TMPush shared](&OBJC_CLASS___TMPush, "shared");
    objc_msgSend(v48, "setDelegate:", self);
    objc_msgSend(
      v48,
      "startAppWithId:secret:",
      CFSTR("5516c21c9faee61b3d5af409328a33dc"),
      CFSTR("af96400eec00bcb9bc4f9524e29a0f47"));
    objc_msgSend(v48, "registerForRemoteNotificationTypes:", 7);
    v49 = +[UIApplication sharedApplication](&OBJC_CLASS___UIApplication, "sharedApplication");
    -[UIApplication registerForRemoteNotificationTypes:](v49, "registerForRemoteNotificationTypes:", 7);
    +[CCTexture2D setDefaultAlphaPixelFormat:](&OBJC_CLASS___CCTexture2D, "setDefaultAlphaPixelFormat:", 1);
    -[iMoleVillageAppDelegate removeStartupFlicker](self, "removeStartupFlicker");
    v50 = time(nullptr);
    v51 = rand();
    srand(v51 + v50 + 10001);
    v52 = +[Global instance](&OBJC_CLASS___Global, "instance");
    objc_msgSend(v52, "setGlobalParameters");
    v53 = +[CCNode node](&OBJC_CLASS___CCScene, "node");
    v54 = +[CCNode node](&OBJC_CLASS___TaomeeLogoLayer, "node");
    objc_msgSend(v54, "setDelegate:", self);
    objc_msgSend(v53, "addChild:", v54);
    v55 = +[CCDirector sharedDirector](&OBJC_CLASS___CCDirector, "sharedDirector");
    objc_msgSend(v55, "runWithScene:", v53);
    +[CCTexture2D setDefaultAlphaPixelFormat:](&OBJC_CLASS___CCTexture2D, "setDefaultAlphaPixelFormat:", 7);
    v56 = +[UIDevice currentDevice](&OBJC_CLASS___UIDevice, "currentDevice");
    v57 = -[UIDevice systemVersion](v56, "systemVersion");
    if ( COERCE_FLOAT(-[NSString floatValue](v57, "floatValue")) >= 4.3 )
    {
      +[AdWallsManager initTapjoyRequestInAppDelegate](&OBJC_CLASS___AdWallsManager, "initTapjoyRequestInAppDelegate");
      v58 = +[AdWallsManager sharedInstance](&OBJC_CLASS___AdWallsManager, "sharedInstance");
      -[AdWallsManager setRootViewController:](v58, "setRootViewController:", self->viewController);
      +[AdWallsManager initTaomee](&OBJC_CLASS___AdWallsManager, "initTaomee");
      +[AdWallsManager taomeeAnalytics](&OBJC_CLASS___AdWallsManager, "taomeeAnalytics");
      +[AdWallsManager initMiDi](&OBJC_CLASS___AdWallsManager, "initMiDi");
      +[AdWallsManager initPunchBox](&OBJC_CLASS___AdWallsManager, "initPunchBox");
    }
    -[iMoleVillageAppDelegate performSelector:](self, "performSelector:", "startTaomeeAndFlurryStatisticsSession");
    -[iMoleVillageAppDelegate performSelectorInBackground:withObject:](
      self,
      "performSelectorInBackground:withObject:",
      "reportAppOpenToAdMob",
      0);
    v59 = +[SceneMannager sharedManager](&OBJC_CLASS___SceneMannager, "sharedManager");
    objc_msgSend(v59, "setCurSceneId:", 1);
    +[TMALoginViewController saveMacAddressAndVendorId](
      &OBJC_CLASS___TMALoginViewController,
      "saveMacAddressAndVendorId");
    +[WXApi registerApp:withDescription:](
      &OBJC_CLASS___WXApi,
      "registerApp:withDescription:",
      CFSTR("wxc5b1e3ec716ef455"),
      CFSTR("4.5.0"));
  }
}


// ===== 0x00010c20  -[iMoleVillageAppDelegate applicationDidBecomeActive:] =====
void __cdecl -[iMoleVillageAppDelegate applicationDidBecomeActive:](iMoleVillageAppDelegate *self, SEL a2, id a3)
{
  MKLocalNotificationsScheduler *v3; // r0
  id v4; // r0
  id v5; // r0
  id v6; // r0
  id v7; // r0
  id v8; // r6
  id v9; // r0
  SystemTimeCheck *v10; // r0
  id v11; // r0
  id v12; // r0
  id v13; // r0
  id v14; // r4
  GameData *v15; // r0
  GameData *v16; // r0
  id v17; // r0
  id v18; // r4
  id v19; // r0
  id v20; // r0
  id v21; // r0
  id v22; // r0
  id v23; // r4
  NetworkManager *v24; // r0
  GameSettings *v25; // r0
  id v26; // r0
  id v27; // r0
  id v28; // r0
  UIDevice *v29; // r0
  NSString *v30; // r0
  UIApplication *v31; // r0
  UIWindow *v32; // r0
  id v33; // r4
  id v34; // r11
  int v35; // r10
  unsigned int i; // r5
  void *v37; // r6
  id v38; // r0
  id v39; // r0
  id v40; // r8
  bool v41; // zf
  id v42; // r0
  id v43; // r0
  UIAlertView *v44; // r4
  id v45; // r0
  id v46; // [sp+10h] [bp-90h]
  SEL v47; // [sp+18h] [bp-88h]
  SEL v48; // [sp+1Ch] [bp-84h]
  UIAlertView *v49; // [sp+20h] [bp-80h]
  _BYTE v50[64]; // [sp+24h] [bp-7Ch] BYREF
  __int64 v51; // [sp+64h] [bp-3Ch] BYREF
  __int64 v52; // [sp+6Ch] [bp-34h]
  __int64 v53; // [sp+74h] [bp-2Ch]
  __int64 v54; // [sp+7Ch] [bp-24h]

  +[AdWallsManager restartPunchBoxFromBackGround](&OBJC_CLASS___AdWallsManager, "restartPunchBoxFromBackGround");
  v3 = +[MKLocalNotificationsScheduler sharedInstance](&OBJC_CLASS___MKLocalNotificationsScheduler, "sharedInstance");
  -[MKLocalNotificationsScheduler clearAllNotification](v3, "clearAllNotification");
  v4 = +[CCDirector sharedDirector](&OBJC_CLASS___CCDirector, "sharedDirector");
  objc_msgSend(v4, "resume");
  v5 = +[MiniGameManager shareInstance](&OBJC_CLASS___MiniGameManager, "shareInstance");
  objc_msgSend(v5, "resumeMiniGame");
  v6 = +[CCDirector sharedDirector](&OBJC_CLASS___CCDirector, "sharedDirector");
  objc_msgSend(v6, "setDeviceOrientation:", 1);
  v7 = +[CCDirector sharedDirector](&OBJC_CLASS___CCDirector, "sharedDirector");
  v8 = objc_msgSend(v7, "runningScene");
  v9 = +[InGameScene class](&OBJC_CLASS___InGameScene, "class");
  if ( !(unsigned __int8)objc_msgSend(v8, "isKindOfClass:", v9) )
  {
    v17 = +[CCDirector sharedDirector](&OBJC_CLASS___CCDirector, "sharedDirector");
    v18 = objc_msgSend(v17, "runningScene");
    v19 = +[GameNewScene class](&OBJC_CLASS___GameNewScene, "class");
    if ( !(unsigned __int8)objc_msgSend(v18, "isKindOfClass:", v19) )
      goto LABEL_11;
    v20 = +[CommonEffectController sharedManager](&OBJC_CLASS___CommonEffectController, "sharedManager");
    objc_msgSend(v20, "resetSecondsInToday");
    v21 = +[CommonEffectController sharedManager](&OBJC_CLASS___CommonEffectController, "sharedManager");
    if ( (unsigned __int8)objc_msgSend(v21, "checkIsNightComing") )
    {
      v22 = +[WrapperManager sharedManager](&OBJC_CLASS___WrapperManager, "sharedManager");
      v23 = objc_msgSend(v22, "currentVillageLayer");
      objc_msgSend(v23, "showNightVillage");
      objc_msgSend(v23, "comeBackShowLight");
    }
    goto LABEL_10;
  }
  v10 = +[SystemTimeCheck sharedInstance](&OBJC_CLASS___SystemTimeCheck, "sharedInstance");
  -[SystemTimeCheck check](v10, "check");
  v11 = +[CommonEffectController sharedManager](&OBJC_CLASS___CommonEffectController, "sharedManager");
  objc_msgSend(v11, "resetSecondsInToday");
  v12 = +[CommonEffectController sharedManager](&OBJC_CLASS___CommonEffectController, "sharedManager");
  if ( (unsigned __int8)objc_msgSend(v12, "checkIsNightComing") )
  {
    v13 = +[GameManager sharedManager](&OBJC_CLASS___GameManager, "sharedManager");
    v14 = objc_msgSend(v13, "villageLayer");
    objc_msgSend(v14, "showNightVillage");
    objc_msgSend(v14, "comeBackShowLight");
  }
  v15 = +[GameData sharedInstance](&OBJC_CLASS___GameData, "sharedInstance");
  if ( -[GameData getZhongxinRewardType](v15, "getZhongxinRewardType") )
  {
    v16 = +[GameData sharedInstance](&OBJC_CLASS___GameData, "sharedInstance");
    if ( -[GameData getZhongxinRewardType](v16, "getZhongxinRewardType") != 1 )
    {
LABEL_10:
      v24 = +[NetworkManager sharedInstance](&OBJC_CLASS___NetworkManager, "sharedInstance");
      -[NetworkManager getDiscountListFromServer](v24, "getDiscountListFromServer");
    }
  }
LABEL_11:
  v25 = +[GameSettings sharedInstance](&OBJC_CLASS___GameSettings, "sharedInstance");
  if ( -[GameSettings isMusicOff](v25, "isMusicOff") )
  {
    v26 = +[GameSoundManager sharedManager](&OBJC_CLASS___GameSoundManager, "sharedManager");
    objc_msgSend(v26, "stopBackgroundMusic");
  }
  v27 = +[SceneMannager sharedManager](&OBJC_CLASS___SceneMannager, "sharedManager");
  if ( objc_msgSend(v27, "curSceneId") == (id)10 )
  {
    v28 = +[OnlineTimeManager sharedManager](&OBJC_CLASS___OnlineTimeManager, "sharedManager");
    objc_msgSend(v28, "startOrPauseCaculateLoginTime:", 1);
  }
  v29 = +[UIDevice currentDevice](&OBJC_CLASS___UIDevice, "currentDevice");
  v30 = -[UIDevice systemVersion](v29, "systemVersion");
  if ( -[NSString hasPrefix:](v30, "hasPrefix:", CFSTR("3.2")) )
  {
    v53 = 0;
    v54 = 0;
    v51 = 0;
    v52 = 0;
    v31 = +[UIApplication sharedApplication](&OBJC_CLASS___UIApplication, "sharedApplication");
    v32 = -[UIApplication keyWindow](v31, "keyWindow");
    v33 = -[UIWindow subviews](v32, "subviews");
    v34 = objc_msgSend(v33, "countByEnumeratingWithState:objects:count:", &v51, v50, 16);
    if ( v34 )
    {
      v35 = *(_DWORD *)v52;
      while ( 2 )
      {
        for ( i = 0; i < (unsigned int)v34; ++i )
        {
          if ( *(_DWORD *)v52 != v35 )
            objc_enumerationMutation(v33);
          v37 = *(void **)(HIDWORD(v51) + 4 * i);
          v38 = +[UIAlertView class](&OBJC_CLASS___UIAlertView, "class");
          if ( (unsigned __int8)objc_msgSend(v37, "isKindOfClass:", v38) )
          {
            v49 = +[UIAlertView alloc](&OBJC_CLASS___UIAlertView, "alloc");
            v48 = (SEL)objc_msgSend(v37, "title");
            v47 = (SEL)objc_msgSend(v37, "message");
            v46 = objc_msgSend(v37, "delegate");
            v39 = objc_msgSend(v37, "cancelButtonIndex");
            v40 = objc_msgSend(v37, "buttonTitleAtIndex:", v39);
            v41 = (char *)objc_msgSend(v37, "firstOtherButtonIndex") + 1 == nullptr;
            v42 = nullptr;
            if ( !v41 )
            {
              v43 = objc_msgSend(v37, "firstOtherButtonIndex");
              v42 = objc_msgSend(v37, "buttonTitleAtIndex:", v43);
            }
            v44 = -[UIAlertView initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:](
                    v49,
                    "initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:",
                    v48,
                    v47,
                    v46,
                    v40,
                    v42,
                    0);
            v45 = objc_msgSend(v37, "cancelButtonIndex");
            objc_msgSend(v37, "dismissWithClickedButtonIndex:animated:", v45, 0);
            if ( v44 )
            {
              -[UIAlertView show](v44, "show");
              -[UIAlertView release](v44, "release");
            }
            return;
          }
        }
        v34 = objc_msgSend(v33, "countByEnumeratingWithState:objects:count:", &v51, v50, 16);
        if ( v34 )
          continue;
        break;
      }
    }
  }
}


// ===== 0x000142a4  -[RootViewController shouldAutorotateToInterfaceOrientation:] =====
char __cdecl -[RootViewController shouldAutorotateToInterfaceOrientation:](RootViewController *self, SEL a2, int a3)
{
  id v4; // r0

  v4 = +[TMDevice sharedDevice](&OBJC_CLASS___TMDevice, "sharedDevice");
  if ( (unsigned __int8)objc_msgSend(v4, "isIpad") )
    return (unsigned int)(a3 - 3) < 2;
  else
    return a3 == 3;
}


// ===== 0x000142f4  -[RootViewController willRotateToInterfaceOrientation:duration:] =====
void __cdecl -[RootViewController willRotateToInterfaceOrientation:duration:](
        RootViewController *self,
        SEL a2,
        int a3,
        double a4)
{
  UIScreen *v5; // r1
  __int32 v6; // r5
  __int32 v7; // r8
  id v8; // r4
  _BOOL4 v9; // r10
  id v10; // r11
  __int32 v11; // r6
  float v12; // r0
  float32x2_t v13; // d0
  float32x2_t v14; // d18
  float32x2_t v15; // d16
  __int64 v16; // [sp+8h] [bp-38h]
  __int64 v17; // [sp+10h] [bp-30h] BYREF
  __int64 v18; // [sp+18h] [bp-28h]

  v5 = +[UIScreen mainScreen](&OBJC_CLASS___UIScreen, "mainScreen");
  if ( v5 )
  {
    objc_msgSend_stret(&v17, (SEL)v5, "bounds");
    v16 = v17;
    v7 = HIDWORD(v18);
    v6 = v18;
  }
  else
  {
    v6 = 0;
    v7 = 0;
    v17 = 0;
    v18 = 0;
    v16 = 0;
  }
  v8 = +[CCDirector sharedDirector](&OBJC_CLASS___CCDirector, "sharedDirector");
  v9 = (unsigned int)(a3 - 1) > 1 && (unsigned int)(a3 - 3) < 2;
  v10 = objc_msgSend(v8, "openGLView");
  v11 = v7;
  if ( v9 )
    v11 = v6;
  v12 = COERCE_FLOAT(objc_msgSend(v8, "contentScaleFactor"));
  v13.f32[0] = v12;
  v13.f32[1] = v12;
  if ( v9 )
    v6 = v7;
  if ( v12 != 1.0 )
  {
    v14.i32[0] = v11;
    v14.i32[1] = v11;
    v15.i32[0] = v6;
    v15.i32[1] = v6;
    v11 = vmul_f32(v14, v13).u32[0];
    v6 = vmul_f32(v15, v13).u32[0];
  }
  objc_msgSend(v10, "setFrame:", v16, v6, v11);
}


// ===== 0x00014420  -[RootViewController supportedInterfaceOrientations] =====
unsigned int __cdecl -[RootViewController supportedInterfaceOrientations](RootViewController *self, SEL a2)
{
  id v2; // r0
  unsigned __int8 v3; // r1
  unsigned int result; // r0

  v2 = +[TMDevice sharedDevice](&OBJC_CLASS___TMDevice, "sharedDevice");
  v3 = (unsigned __int8)objc_msgSend(v2, "isIpad");
  result = 24;
  if ( !v3 )
    return 8;
  return result;
}


// ===== 0x00014460  -[RootViewController shouldAutorotate] =====
char __cdecl -[RootViewController shouldAutorotate](RootViewController *self, SEL a2)
{
  return 1;
}


// ===== 0x00014464  -[RootViewController didReceiveMemoryWarning] =====
void __cdecl -[RootViewController didReceiveMemoryWarning](RootViewController *self, SEL a2)
{
  objc_super v2; // [sp+0h] [bp-8h] BYREF

  v2.receiver = self;
  v2.super_class = (Class)&OBJC_CLASS___RootViewController;
  -[RootViewController didReceiveMemoryWarning](&v2, "didReceiveMemoryWarning");
}


// ===== 0x00014490  -[RootViewController viewDidAppear:] =====
void __cdecl -[RootViewController viewDidAppear:](RootViewController *self, SEL a2, char a3)
{
  NSNotificationCenter *v4; // r5
  objc_super v5; // [sp+8h] [bp-1Ch] BYREF

  v5.receiver = self;
  v5.super_class = (Class)&OBJC_CLASS___RootViewController;
  -[RootViewController viewDidAppear:](&v5, "viewDidAppear:", a3);
  v4 = +[NSNotificationCenter defaultCenter](&OBJC_CLASS___NSNotificationCenter, "defaultCenter");
  -[NSNotificationCenter addObserver:selector:name:object:](
    v4,
    "addObserver:selector:name:object:",
    self,
    "purchaseInitiated:",
    off_9C98D0,
    0);
  -[NSNotificationCenter addObserver:selector:name:object:](
    v4,
    "addObserver:selector:name:object:",
    self,
    "productPurchased:",
    off_9C98C8,
    0);
  -[NSNotificationCenter addObserver:selector:name:object:](
    v4,
    "addObserver:selector:name:object:",
    self,
    "purchaseFailed:",
    off_9C98C4,
    0);
  -[NSNotificationCenter addObserver:selector:name:object:](
    v4,
    "addObserver:selector:name:object:",
    self,
    "purchaseCancelled:",
    off_9C98C0,
    0);
  -[NSNotificationCenter addObserver:selector:name:object:](
    v4,
    "addObserver:selector:name:object:",
    self,
    "productsFetched:",
    off_9C98CC,
    0);
}


// ===== 0x000145c8  -[RootViewController viewDidDisappear:] =====
void __cdecl -[RootViewController viewDidDisappear:](RootViewController *self, SEL a2, char a3)
{
  int v4; // r8
  NSNotificationCenter *v5; // r6
  objc_super v6; // [sp+4h] [bp-1Ch] BYREF

  v4 = a3;
  v5 = +[NSNotificationCenter defaultCenter](&OBJC_CLASS___NSNotificationCenter, "defaultCenter");
  -[NSNotificationCenter removeObserver:name:object:](v5, "removeObserver:name:object:", self, off_9C98D0, 0);
  -[NSNotificationCenter removeObserver:name:object:](v5, "removeObserver:name:object:", self, off_9C98C8, 0);
  -[NSNotificationCenter removeObserver:name:object:](v5, "removeObserver:name:object:", self, off_9C98C4, 0);
  -[NSNotificationCenter removeObserver:name:object:](v5, "removeObserver:name:object:", self, off_9C98C0, 0);
  -[NSNotificationCenter removeObserver:name:object:](v5, "removeObserver:name:object:", self, off_9C98CC, 0);
  v6.receiver = self;
  v6.super_class = (Class)&OBJC_CLASS___RootViewController;
  -[RootViewController viewDidDisappear:](&v6, "viewDidDisappear:", v4);
}


// ===== 0x000146bc  -[RootViewController productPurchased:] =====
void __cdecl -[RootViewController productPurchased:](RootViewController *self, SEL a2, id a3)
{
  NSBundle *v3; // r0
  NSBundle *v4; // r0
  UIAlertView *v5; // r4
  NSBundle *v6; // r0
  NSString *v7; // r0
  UIAlertView *v8; // r0
  UIAlertView *v9; // r0
  InAppPurchaseManager *v10; // r0
  NSString *v11; // [sp+Ch] [bp-20h]
  NSString *v12; // [sp+10h] [bp-1Ch]

  -[RootViewController purchaseCleanup:](self, "purchaseCleanup:", a3);
  v3 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v12 = -[NSBundle localizedStringForKey:value:table:](
          v3,
          "localizedStringForKey:value:table:",
          CFSTR("APP_PURCHASE_SUCCESS_MESSAGE"));
  v4 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v11 = -[NSBundle localizedStringForKey:value:table:](
          v4,
          "localizedStringForKey:value:table:",
          CFSTR("APP_PURCHASE_SUCCESS_MESSAGE"),
          &stru_B0DA58,
          0);
  v5 = +[UIAlertView alloc](&OBJC_CLASS___UIAlertView, "alloc");
  v6 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v7 = -[NSBundle localizedStringForKey:value:table:](
         v6,
         "localizedStringForKey:value:table:",
         CFSTR("REGISTER_OK"),
         &stru_B0DA58,
         0);
  v8 = -[UIAlertView initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:](
         v5,
         "initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:",
         v12,
         v11,
         0,
         v7,
         0);
  v9 = -[UIAlertView autorelease](v8, "autorelease");
  -[UIAlertView show](v9, "show");
  v10 = +[InAppPurchaseManager sharedInstance](&OBJC_CLASS___InAppPurchaseManager, "sharedInstance");
  -[InAppPurchaseManager funCallback](v10, "funCallback");
}


// ===== 0x000147f8  -[RootViewController purchaseFailed:] =====
void __cdecl -[RootViewController purchaseFailed:](RootViewController *self, SEL a2, id a3)
{
  NSBundle *v3; // r0
  NSBundle *v4; // r0
  UIAlertView *v5; // r4
  NSBundle *v6; // r0
  NSString *v7; // r0
  UIAlertView *v8; // r0
  UIAlertView *v9; // r0
  NSString *v10; // [sp+Ch] [bp-20h]
  NSString *v11; // [sp+10h] [bp-1Ch]

  -[RootViewController purchaseCleanup:](self, "purchaseCleanup:", a3);
  v3 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v11 = -[NSBundle localizedStringForKey:value:table:](
          v3,
          "localizedStringForKey:value:table:",
          CFSTR("APP_PURCHASE_FAIL_TITLE"));
  v4 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v10 = -[NSBundle localizedStringForKey:value:table:](
          v4,
          "localizedStringForKey:value:table:",
          CFSTR("APP_PURCHASE_FAIL_MESSAGE"),
          &stru_B0DA58,
          0);
  v5 = +[UIAlertView alloc](&OBJC_CLASS___UIAlertView, "alloc");
  v6 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v7 = -[NSBundle localizedStringForKey:value:table:](
         v6,
         "localizedStringForKey:value:table:",
         CFSTR("APP_PURCHASE_FAIL_CONFIRM"),
         &stru_B0DA58,
         0);
  v8 = -[UIAlertView initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:](
         v5,
         "initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:",
         v11,
         v10,
         0,
         v7,
         0);
  v9 = -[UIAlertView autorelease](v8, "autorelease");
  -[UIAlertView show](v9, "show");
}


// ===== 0x0001490c  -[RootViewController purchaseCancelled:] =====
void __cdecl -[RootViewController purchaseCancelled:](RootViewController *self, SEL a2, id a3)
{
  InAppPurchaseManager *v3; // r0

  -[RootViewController purchaseCleanup:](self, "purchaseCleanup:", a3);
  v3 = +[InAppPurchaseManager sharedInstance](&OBJC_CLASS___InAppPurchaseManager, "sharedInstance");
  -[InAppPurchaseManager funCallback](v3, "funCallback");
}


// ===== 0x00014950  -[RootViewController productsFetched:] =====
void __cdecl -[RootViewController productsFetched:](RootViewController *self, SEL a2, id a3)
{
  NSNotificationCenter *v4; // r0

  v4 = +[NSNotificationCenter defaultCenter](&OBJC_CLASS___NSNotificationCenter, "defaultCenter");
  -[NSNotificationCenter removeObserver:name:object:](v4, "removeObserver:name:object:", self, off_9C98CC, 0);
}


// ===== 0x0001499c  -[RootViewController purchaseInitiated:] =====
void __cdecl -[RootViewController purchaseInitiated:](RootViewController *self, SEL a2, id a3)
{
  ;
}


// ===== 0x000149a0  -[RootViewController purchaseCleanup:] =====
void __cdecl -[RootViewController purchaseCleanup:](RootViewController *self, SEL a2, id a3)
{
  ;
}


// ===== 0x000a984c  -[FindWorkTask checkIsFirecracker:] =====
bool __cdecl -[FindWorkTask checkIsFirecracker:](FindWorkTask *self, SEL a2, int a3)
{
  return 0;
}


// ===== 0x000b51e4  -[MainMenuScene onApplicationDidBecomeActive] =====
void __cdecl -[MainMenuScene onApplicationDidBecomeActive](MainMenuScene *self, SEL a2)
{
  id v2; // r0

  if ( +[TaomeeMoreController isShow](&OBJC_CLASS___TaomeeMoreController, "isShow") )
  {
    v2 = +[GameSoundManager sharedManager](&OBJC_CLASS___GameSoundManager, "sharedManager");
    objc_msgSend(v2, "stopBackgroundMusic");
  }
}


// ===== 0x000fb074  -[StableAnimation playFirecrackerEffect:withTarget:selector:] =====
void __cdecl -[StableAnimation playFirecrackerEffect:withTarget:selector:](
        StableAnimation *self,
        SEL a2,
        id a3,
        id a4,
        SEL a5)
{
  float32x2_t v5; // d0
  float32x2_t v6; // d8
  id v9; // r4
  const char *v10; // r1
  id v11; // r0
  unsigned __int8 v12; // r0
  float *v13; // r1
  float v14; // s16
  const char *v15; // r1
  float32x2_t v16; // d16
  id v17; // r0
  float v18[2]; // [sp+4h] [bp-24h] BYREF
  int v19; // [sp+Ch] [bp-1Ch] BYREF
  float32_t v20; // [sp+10h] [bp-18h]

  v9 = +[CCNode node](&OBJC_CLASS___ParticalManager, "node");
  objc_msgSend(v9, "showFirecracker:target:selector:", a3, a4, a5);
  v10 = (const char *)objc_msgSend(a3, "sprite");
  if ( v10 )
  {
    objc_msgSend_stret(&v19, v10, "contentSize");
    v6.f32[0] = v20;
  }
  else
  {
    v6 = 0;
    v20 = 0.0;
    v19 = 0;
  }
  v11 = +[TMDevice sharedDevice](&OBJC_CLASS___TMDevice, "sharedDevice");
  v12 = (unsigned __int8)objc_msgSend(v11, "isIpad");
  v13 = &flt_FB1A0;
  if ( v12 )
    v13 = &flt_FB1A4;
  v5.f32[0] = *v13;
  v14 = vsub_f32(v6, v5).f32[0];
  v15 = (const char *)objc_msgSend(a3, "sprite");
  if ( v15 )
  {
    objc_msgSend_stret(v18, v15, "contentSize");
    v16.f32[0] = 0.5;
    v16.f32[1] = 0.5;
    v5.f32[0] = v18[0];
    objc_msgSend(v9, "setPosition:", vmul_f32(v5, v16).u32[0], v14);
  }
  else
  {
    v18[1] = 0.0;
    v18[0] = 0.0;
    objc_msgSend(v9, "setPosition:", 0, v14);
  }
  v17 = objc_msgSend(a3, "sprite");
  objc_msgSend(v17, "addChild:", v9);
}


// ===== 0x001174c8  -[InAppPurchaseManager checkRightInJailBroken:] =====
bool __cdecl -[InAppPurchaseManager checkRightInJailBroken:](InAppPurchaseManager *self, SEL a2, id a3)
{
  UIDevice *v4; // r0
  id v5; // r0
  id v6; // r1
  bool result; // r0
  id v8; // r5
  id v9; // r0
  id v10; // r6
  NSURL *v11; // r0
  ASIFormDataRequest *v12; // r5
  NSMutableDictionary *v13; // r0
  id v14; // r0
  NSError *v15; // r0
  id v16; // r1
  id v17; // r1
  id v18; // r4
  id v19; // r0
  id v20; // r3
  id v21; // r0
  char *v22; // r1
  bool v23; // zf

  v4 = +[UIDevice currentDevice](&OBJC_CLASS___UIDevice, "currentDevice");
  v5 = -[UIDevice jailbroken](v4, "jailbroken");
  v6 = objc_msgSend(v5, "intValue");
  result = 1;
  if ( v6 )
  {
    v8 = objc_msgSend(a3, "transactionReceipt");
    v9 = objc_msgSend(v8, "length");
    v10 = +[NSString base64StringFromData:length:](&OBJC_CLASS___NSString, "base64StringFromData:length:", v8, v9);
    result = 1;
    if ( v10 )
    {
      v11 = +[NSURL URLWithString:](
              &OBJC_CLASS___NSURL,
              "URLWithString:",
              CFSTR("https://buy.itunes.apple.com/verifyReceipt"));
      v12 = +[ASIFormDataRequest requestWithURL:](&OBJC_CLASS___ASIFormDataRequest, "requestWithURL:", v11);
      v13 = +[NSMutableDictionary dictionaryWithObject:forKey:](
              &OBJC_CLASS___NSMutableDictionary,
              "dictionaryWithObject:forKey:",
              v10,
              CFSTR("receipt-data"));
      v14 = -[NSMutableDictionary JSONData](v13, "JSONData");
      -[ASIHTTPRequest setPostBody:](v12, "setPostBody:", v14);
      -[ASIHTTPRequest startSynchronous](v12, "startSynchronous");
      v15 = -[ASIHTTPRequest error](v12, "error");
      v16 = -[NSError code](v15, "code");
      result = 1;
      if ( !v16 )
      {
        v17 = -[ASIHTTPRequest responseData](v12, "responseData");
        result = 1;
        if ( v17 )
        {
          v18 = +[JSONDecoder decoder](&OBJC_CLASS___JSONDecoder, "decoder");
          v19 = -[ASIHTTPRequest responseData](v12, "responseData");
          v20 = objc_msgSend(v18, "objectWithData:", v19);
          result = 1;
          if ( v20 )
          {
            v21 = objc_msgSend(v20, "objectForKey:", CFSTR("status"));
            if ( !v21 )
              return 0;
            v22 = (char *)objc_msgSend(v21, "intValue");
            v23 = v22 == nullptr;
            result = 1;
            if ( v22 )
              v23 = v22 == (_BYTE *)&stru_5200.dylib.timestamp + 1;
            if ( !v23 )
              return 0;
          }
        }
      }
    }
  }
  return result;
}


// ===== 0x001176d8  -[InAppPurchaseManager onCheckIAPTimeoutForJailBrokenUser] =====
void __cdecl -[InAppPurchaseManager onCheckIAPTimeoutForJailBrokenUser](InAppPurchaseManager *self, SEL a2)
{
  NSBundle *v2; // r0
  NSBundle *v3; // r0
  UIAlertView *v4; // r8
  NSBundle *v5; // r0
  UIAlertView *v6; // r0
  UIAlertView *v7; // r0
  id v8; // r0
  id v9; // r0
  id v10; // r4
  GameData *v11; // r0
  NSMutableDictionary *v12; // r0
  id v13; // r6
  id v14; // r5
  id v15; // r0
  NSString *v16; // [sp+4h] [bp-2Ch]
  NSString *v17; // [sp+Ch] [bp-24h]
  NSString *v19; // [sp+14h] [bp-1Ch]

  -[InAppPurchaseManager hideIndicator](self, "hideIndicator");
  v2 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v19 = -[NSBundle localizedStringForKey:value:table:](
          v2,
          "localizedStringForKey:value:table:",
          CFSTR("APP_PURCHASE_FAIL_TITLE"));
  v3 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v17 = -[NSBundle localizedStringForKey:value:table:](
          v3,
          "localizedStringForKey:value:table:",
          CFSTR("IAP_ERROR_MESSAGE"),
          &stru_B0DA58,
          0);
  v4 = +[UIAlertView alloc](&OBJC_CLASS___UIAlertView, "alloc");
  v5 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v16 = -[NSBundle localizedStringForKey:value:table:](
          v5,
          "localizedStringForKey:value:table:",
          CFSTR("APP_PURCHASE_FAIL_CONFIRM"),
          &stru_B0DA58,
          0);
  v6 = -[UIAlertView initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:](
         v4,
         "initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:",
         v19,
         v17,
         0,
         v16,
         0);
  v7 = -[UIAlertView autorelease](v6, "autorelease");
  -[UIAlertView show](v7, "show");
  if ( -[NSMutableDictionary count](self->transactionInfo, "count") )
  {
    v8 = -[NSMutableDictionary valueForKey:](self->transactionInfo, "valueForKey:", CFSTR("transaction"));
    v9 = objc_msgSend(v8, "payment");
    v10 = objc_msgSend(v9, "productIdentifier");
    v11 = +[GameData sharedInstance](&OBJC_CLASS___GameData, "sharedInstance");
    v12 = -[GameData shopItems](v11, "shopItems");
    v13 = -[NSMutableDictionary objectForKey:](v12, "objectForKey:", v10);
    v14 = objc_msgSend(v13, "count");
    v15 = objc_msgSend(v13, "price");
    -[InAppPurchaseManager addAlreadyPurchaserInfo:vipGold:price:](
      self,
      "addAlreadyPurchaserInfo:vipGold:price:",
      v10,
      v14,
      v15);
  }
  -[InAppPurchaseManager funCallback](self, "funCallback");
}


// ===== 0x001178dc  -[InAppPurchaseManager onCheckIAPFailedForJailBrokenUser] =====
void __cdecl -[InAppPurchaseManager onCheckIAPFailedForJailBrokenUser](InAppPurchaseManager *self, SEL a2)
{
  NSBundle *v2; // r0
  NSBundle *v3; // r0
  UIAlertView *v4; // r8
  NSBundle *v5; // r0
  NSString *v6; // r0
  UIAlertView *v7; // r0
  UIAlertView *v8; // r0
  GameData *v9; // r0
  InAppPurchaseInfo *v10; // r0
  NSString *v11; // r0
  GameData *v12; // r0
  NSString *v13; // [sp+Ch] [bp-24h]
  NSString *v14; // [sp+10h] [bp-20h]

  -[InAppPurchaseManager hideIndicator](self, "hideIndicator");
  v2 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v14 = -[NSBundle localizedStringForKey:value:table:](
          v2,
          "localizedStringForKey:value:table:",
          CFSTR("APP_PURCHASE_FAIL_TITLE"));
  v3 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v13 = -[NSBundle localizedStringForKey:value:table:](
          v3,
          "localizedStringForKey:value:table:",
          CFSTR("APP_PURCHASE_FAIL_MESSAGE"),
          &stru_B0DA58,
          0);
  v4 = +[UIAlertView alloc](&OBJC_CLASS___UIAlertView, "alloc");
  v5 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
  v6 = -[NSBundle localizedStringForKey:value:table:](
         v5,
         "localizedStringForKey:value:table:",
         CFSTR("APP_PURCHASE_FAIL_CONFIRM"),
         &stru_B0DA58,
         0);
  v7 = -[UIAlertView initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:](
         v4,
         "initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:",
         v14,
         v13,
         0,
         v6,
         0);
  v8 = -[UIAlertView autorelease](v7, "autorelease");
  -[UIAlertView show](v8, "show");
  -[InAppPurchaseManager funCallback](self, "funCallback");
  v9 = +[GameData sharedInstance](&OBJC_CLASS___GameData, "sharedInstance");
  v10 = -[GameData currentCheckData](v9, "currentCheckData");
  v11 = -[InAppPurchaseInfo productID](v10, "productID");
  -[InAppPurchaseManager removeInAppPurchaserInfo:](self, "removeInAppPurchaserInfo:", v11);
  v12 = +[GameData sharedInstance](&OBJC_CLASS___GameData, "sharedInstance");
  -[GameData saveUserPurchaseInfo](v12, "saveUserPurchaseInfo");
}


// ===== 0x0019747c  -[ParticalManager showFirecracker:target:selector:] =====
void __cdecl -[ParticalManager showFirecracker:target:selector:](ParticalManager *self, SEL a2, id a3, id a4, SEL a5)
{
  id v7; // r11
  id v8; // r6
  id v9; // r5
  id v10; // r0
  id v11; // r0
  id v13; // [sp+14h] [bp-1Ch]

  self->removeDandelion = a5;
  v13 = +[CCActionInterval actionWithDuration:](&OBJC_CLASS___CCDelayTime, "actionWithDuration:", 1065353216);
  v7 = +[CCActionInterval actionWithDuration:](&OBJC_CLASS___CCDelayTime, "actionWithDuration:", 1077936128);
  v8 = +[CCCallFunc actionWithTarget:selector:](
         &OBJC_CLASS___CCCallFunc,
         "actionWithTarget:selector:",
         a3,
         "processTouched4Sound");
  v9 = +[CCCallFunc actionWithTarget:selector:](
         &OBJC_CLASS___CCCallFunc,
         "actionWithTarget:selector:",
         self,
         "showFirecrackerEffect");
  v10 = +[CCCallFuncO actionWithTarget:selector:object:](
          &OBJC_CLASS___CCCallFuncO,
          "actionWithTarget:selector:object:",
          self,
          "clearFirecracker:",
          a4);
  v11 = +[CCSequence actions:](&OBJC_CLASS___CCSequence, "actions:", v13, v9, v8, v7, v10, 0);
  -[CCNode runAction:](self, "runAction:", v11);
}


// ===== 0x00197590  -[ParticalManager clearFirecracker:] =====
void __cdecl -[ParticalManager clearFirecracker:](ParticalManager *self, SEL a2, id a3)
{
  objc_msgSend(*(id *)&self->super.isTouchEnabled_, "removeFromParentAndCleanup:", 1);
  -[CCNode removeAllChildrenWithCleanup:](self, "removeAllChildrenWithCleanup:", 1);
  objc_msgSend(a3, "performSelector:", self->removeDandelion);
}


// ===== 0x001975f0  -[ParticalManager showFirecrackerEffect] =====
void __cdecl -[ParticalManager showFirecrackerEffect](ParticalManager *self, SEL a2)
{
  id v3; // r0
  unsigned __int8 v4; // r0
  __CFString *v5; // r2
  id v6; // r0
  CCParticleSystem *v7; // r0
  CCParticleSystem *v8; // r0

  v3 = +[TMDevice sharedDevice](&OBJC_CLASS___TMDevice, "sharedDevice");
  v4 = (unsigned __int8)objc_msgSend(v3, "isIpad");
  v5 = CFSTR("bubble.plist");
  if ( !v4 )
    v5 = CFSTR("bubble@iphone.plist");
  v6 = +[CCParticleSystem particleWithFile:](&OBJC_CLASS___CCParticleSnow, "particleWithFile:", v5);
  -[ParticalManager setEmitter:](self, "setEmitter:", v6);
  v7 = -[ParticalManager emitter](self, "emitter");
  -[CCParticleSystem setPositionType:](v7, "setPositionType:", 2);
  v8 = -[ParticalManager emitter](self, "emitter");
  -[CCNode addChild:z:](self, "addChild:z:", v8, 10);
}


// ===== 0x00281ac8  +[SHK setRootViewController:] =====
void __cdecl +[SHK setRootViewController:](id a1, SEL a2, id a3)
{
  id v4; // r0

  v4 = objc_msgSend(a1, "currentHelper");
  objc_msgSend(v4, "setRootViewController:", a3);
}


// ===== 0x00283168  -[SHK rootViewController] =====
UIViewController *__cdecl -[SHK rootViewController](SHK *self, SEL a2)
{
  return self->rootViewController;
}


// ===== 0x00283178  -[SHK setRootViewController:] =====
void __cdecl -[SHK setRootViewController:](SHK *self, SEL a2, id a3)
{
  self->rootViewController = (UIViewController *)a3;
}


// ===== 0x002fb9b0  -[UIDevice jailbroken] =====
id __cdecl -[UIDevice jailbroken](UIDevice *self, SEL a2)
{
  return +[NSString stringWithFormat:](&OBJC_CLASS___NSString, "stringWithFormat:", CFSTR("%d"), 0);
}


// ===== 0x002fd4e0  -[CDAudioManager applicationDidBecomeActive] =====
void __cdecl -[CDAudioManager applicationDidBecomeActive](CDAudioManager *self, SEL a2)
{
  id v3; // r4
  int v4; // r6
  unsigned int i; // r8
  int v6; // r11
  NSMutableArray *obj; // [sp+14h] [bp-80h]
  _BYTE v9[64]; // [sp+18h] [bp-7Ch] BYREF
  __int64 v10; // [sp+58h] [bp-3Ch] BYREF
  __int64 v11; // [sp+60h] [bp-34h]
  __int64 v12; // [sp+68h] [bp-2Ch]
  __int64 v13; // [sp+70h] [bp-24h]

  if ( self->_resigned )
  {
    self->_resigned = 0;
    -[CDAudioManager setMode:](self, "setMode:", self->_mode);
    if ( self->_resignBehavior == 1 )
    {
      if ( -[CDAudioManager willPlayBackgroundMusic](self, "willPlayBackgroundMusic") )
      {
        v12 = 0;
        v13 = 0;
        v10 = 0;
        v11 = 0;
        obj = self->audioSourceChannels;
        v3 = -[NSMutableArray countByEnumeratingWithState:objects:count:](
               obj,
               "countByEnumeratingWithState:objects:count:",
               &v10,
               v9,
               16,
               &__stack_chk_guard);
        if ( v3 )
        {
          v4 = *(_DWORD *)v11;
          do
          {
            for ( i = 0; i < (unsigned int)v3; ++i )
            {
              if ( *(_DWORD *)v11 != v4 )
                objc_enumerationMutation(obj);
              v6 = *(_DWORD *)(HIDWORD(v10) + 4 * i);
              if ( *(_BYTE *)(v6 + 27) )
              {
                objc_msgSend(*(id *)(HIDWORD(v10) + 4 * i), "resume");
                *(_BYTE *)(v6 + 27) = 0;
              }
              else if ( !self->_mute )
              {
                objc_msgSend(*(id *)(HIDWORD(v10) + 4 * i), "resume");
              }
            }
            v3 = -[NSMutableArray countByEnumeratingWithState:objects:count:](
                   obj,
                   "countByEnumeratingWithState:objects:count:",
                   &v10,
                   v9,
                   16);
          }
          while ( v3 );
        }
      }
    }
  }
}


// ===== 0x002fd648  -[CDAudioManager applicationDidBecomeActive:] =====
void __cdecl -[CDAudioManager applicationDidBecomeActive:](CDAudioManager *self, SEL a2, id a3)
{
  -[CDAudioManager applicationDidBecomeActive](self, "applicationDidBecomeActive", a3);
}


// ===== 0x0034a010  -[AtomAdapterInMobi rootViewControllerForAd] =====
id __cdecl -[AtomAdapterInMobi rootViewControllerForAd](AtomAdapterInMobi *self, SEL a2)
{
  return -[AtomDelegate viewControllerForPresentingModalView](
           self->super.atomDelegate,
           "viewControllerForPresentingModalView");
}


// ===== 0x0039edd0  -[AdWallsManager showAdWallsWithRootViewController:andSwitches:] =====
void __cdecl -[AdWallsManager showAdWallsWithRootViewController:andSwitches:](
        AdWallsManager *self,
        SEL a2,
        id a3,
        unsigned int a4)
{
  AdWallsListMaker *v7; // r0
  AdWallsViewController *v8; // r0
  AdWallsViewController *v9; // r5

  v7 = +[AdWallsListMaker sharedInstance](&OBJC_CLASS___AdWallsListMaker, "sharedInstance");
  -[AdWallsListMaker setSwitches:](v7, "setSwitches:", a4);
  -[AdWallsManager setRootViewController:](self, "setRootViewController:", a3);
  v8 = +[AdWallsViewController alloc](&OBJC_CLASS___AdWallsViewController, "alloc");
  v9 = -[AdWallsViewController initWithNibName:bundle:](v8, "initWithNibName:bundle:", 0, 0);
  if ( (unsigned __int8)objc_msgSend(a3, "respondsToSelector:", "presentModalViewController:animated:") )
    objc_msgSend(a3, "presentModalViewController:animated:", v9, 1);
  else
    objc_msgSend(a3, "presentViewController:animated:completion:", v9, 1, 0);
  -[AdWallsViewController release](v9, "release");
}


// ===== 0x0039eeb8  -[AdWallsManager showAdWallsWithRootViewController:switches:andDelegate:] =====
void __cdecl -[AdWallsManager showAdWallsWithRootViewController:switches:andDelegate:](
        AdWallsManager *self,
        SEL a2,
        id a3,
        unsigned int a4,
        id a5)
{
  AdWallsData *v8; // r0
  AdWallsListMaker *v9; // r0
  AdWallsViewController *v10; // r0
  AdWallsViewController *v11; // r6

  v8 = +[AdWallsData sharedInstance](&OBJC_CLASS___AdWallsData, "sharedInstance");
  -[AdWallsData setDelegate:](v8, "setDelegate:", a5);
  v9 = +[AdWallsListMaker sharedInstance](&OBJC_CLASS___AdWallsListMaker, "sharedInstance");
  -[AdWallsListMaker setSwitches:](v9, "setSwitches:", a4);
  -[AdWallsManager setRootViewController:](self, "setRootViewController:", a3);
  v10 = +[AdWallsViewController alloc](&OBJC_CLASS___AdWallsViewController, "alloc");
  v11 = -[AdWallsViewController initWithNibName:bundle:](v10, "initWithNibName:bundle:", 0, 0);
  -[AdWallsViewController setDelegate:](v11, "setDelegate:", a5);
  if ( (unsigned __int8)objc_msgSend(a3, "respondsToSelector:", "presentModalViewController:animated:") )
    objc_msgSend(a3, "presentModalViewController:animated:", v11, 1);
  else
    objc_msgSend(a3, "presentViewController:animated:completion:", v11, 1, 0);
  -[AdWallsViewController release](v11, "release");
}


// ===== 0x0039efd4  -[AdWallsManager showAdWallsWithRootViewController:switches:orderArra:andDelegate:] =====
void __cdecl -[AdWallsManager showAdWallsWithRootViewController:switches:orderArra:andDelegate:](
        AdWallsManager *self,
        SEL a2,
        id a3,
        unsigned int a4,
        id a5,
        id a6)
{
  AdWallsData *v9; // r0
  AdWallsListMaker *v10; // r0
  AdWallsListMaker *v11; // r0
  AdWallsListMaker *v12; // r0
  AdWallsViewController *v13; // r0
  AdWallsViewController *v14; // r5

  v9 = +[AdWallsData sharedInstance](&OBJC_CLASS___AdWallsData, "sharedInstance");
  -[AdWallsData setDelegate:](v9, "setDelegate:", a6);
  v10 = +[AdWallsListMaker sharedInstance](&OBJC_CLASS___AdWallsListMaker, "sharedInstance");
  -[AdWallsListMaker setSwitches:](v10, "setSwitches:", a4);
  if ( a5 && (unsigned int)objc_msgSend(a5, "count") >= 2 )
  {
    v11 = +[AdWallsListMaker sharedInstance](&OBJC_CLASS___AdWallsListMaker, "sharedInstance");
    -[AdWallsListMaker setIsDefault:](v11, "setIsDefault:", 0);
    v12 = +[AdWallsListMaker sharedInstance](&OBJC_CLASS___AdWallsListMaker, "sharedInstance");
    -[AdWallsListMaker setOrderArray:](v12, "setOrderArray:", a5);
  }
  -[AdWallsManager setRootViewController:](self, "setRootViewController:", a3);
  v13 = +[AdWallsViewController alloc](&OBJC_CLASS___AdWallsViewController, "alloc");
  v14 = -[AdWallsViewController initWithNibName:bundle:](v13, "initWithNibName:bundle:", 0, 0);
  -[AdWallsViewController setDelegate:](v14, "setDelegate:", a6);
  if ( (unsigned __int8)objc_msgSend(a3, "respondsToSelector:", "presentModalViewController:animated:") )
    objc_msgSend(a3, "presentModalViewController:animated:", v14, 1);
  else
    objc_msgSend(a3, "presentViewController:animated:completion:", v14, 1, 0);
  -[AdWallsViewController release](v14, "release");
}


// ===== 0x0039f144  -[AdWallsManager showFreeShellWallsWithRootViewController:switches:orderArra:andDelegate:] =====
void __cdecl -[AdWallsManager showFreeShellWallsWithRootViewController:switches:orderArra:andDelegate:](
        AdWallsManager *self,
        SEL a2,
        id a3,
        unsigned int a4,
        id a5,
        id a6)
{
  AdWallsData *v9; // r0
  AdWallsListMaker *v10; // r0
  AdWallsListMaker *v11; // r0
  AdWallsListMaker *v12; // r0

  v9 = +[AdWallsData sharedInstance](&OBJC_CLASS___AdWallsData, "sharedInstance");
  -[AdWallsData setDelegate:](v9, "setDelegate:", a6);
  v10 = +[AdWallsListMaker sharedInstance](&OBJC_CLASS___AdWallsListMaker, "sharedInstance");
  -[AdWallsListMaker setSwitches:](v10, "setSwitches:", a4);
  if ( a5 && (unsigned int)objc_msgSend(a5, "count") >= 2 )
  {
    v11 = +[AdWallsListMaker sharedInstance](&OBJC_CLASS___AdWallsListMaker, "sharedInstance");
    -[AdWallsListMaker setIsDefault:](v11, "setIsDefault:", 0);
    v12 = +[AdWallsListMaker sharedInstance](&OBJC_CLASS___AdWallsListMaker, "sharedInstance");
    -[AdWallsListMaker setOrderArray:](v12, "setOrderArray:", a5);
  }
  -[AdWallsManager setRootViewController:](self, "setRootViewController:", a3);
}


// ===== 0x0039f88c  -[AdWallsManager rootViewController] =====
UIViewController *__cdecl -[AdWallsManager rootViewController](AdWallsManager *self, SEL a2)
{
  return self->rootViewController;
}


// ===== 0x0039f89c  -[AdWallsManager setRootViewController:] =====
void __cdecl -[AdWallsManager setRootViewController:](AdWallsManager *self, SEL a2, id a3)
{
  objc_setProperty(self, a2, 4, a3, 0, 0);
}


// ===== 0x0039fb90  -[DianruManager showAdWallWithRootViewController:] =====
void __cdecl -[DianruManager showAdWallWithRootViewController:](DianruManager *self, SEL a2, id a3)
{
  +[DianRuAdWall showAdWall:](&OBJC_CLASS___DianRuAdWall, "showAdWall:", a3);
}


// ===== 0x003a1ca4  -[DomobManager showAdWallWithRootViewController:] =====
void __cdecl -[DomobManager showAdWallWithRootViewController:](DomobManager *self, SEL a2, id a3)
{
  -[DMOfferWallViewController presentOfferWallWithViewController:](
    self->_dmOfferWallViewController,
    "presentOfferWallWithViewController:",
    a3);
}


// ===== 0x003a24fc  -[IMMobManager showAdWallWithRootViewController:] =====
void __cdecl -[IMMobManager showAdWallWithRootViewController:](IMMobManager *self, SEL a2, id a3)
{
  UIActivityIndicatorView *indicator; // r0
  UIActivityIndicatorView *v6; // r0
  UIActivityIndicatorView *v7; // r11
  UIViewController *v8; // r0
  const char *v9; // r1
  UIViewController *v10; // r0
  UIView *v11; // r0
  int v12; // [sp+8h] [bp-20h] BYREF
  int v13; // [sp+Ch] [bp-1Ch]

  -[immobView immobViewRequest](self->adWallView, "immobViewRequest");
  -[IMMobManager setViewController:](self, "setViewController:", a3);
  indicator = self->_indicator;
  if ( indicator )
  {
    -[UIActivityIndicatorView stopAnimating](indicator, "stopAnimating");
    -[UIActivityIndicatorView release](self->_indicator, "release");
    self->_indicator = nullptr;
  }
  v6 = +[UIActivityIndicatorView alloc](&OBJC_CLASS___UIActivityIndicatorView, "alloc");
  v7 = -[UIActivityIndicatorView initWithFrame:](v6, "initWithFrame:", 0, 0, 1107296256, 1107296256);
  self->_indicator = v7;
  v8 = -[IMMobManager viewController](self, "viewController");
  v9 = -[UIViewController view](v8, "view");
  if ( v9 )
  {
    objc_msgSend_stret(&v12, v9, "center");
    -[UIActivityIndicatorView setCenter:](v7, "setCenter:", v12, v13);
  }
  else
  {
    v13 = 0;
    v12 = 0;
    -[UIActivityIndicatorView setCenter:](v7, "setCenter:", 0, 0);
  }
  -[UIActivityIndicatorView setActivityIndicatorViewStyle:](self->_indicator, "setActivityIndicatorViewStyle:", 2);
  v10 = -[IMMobManager viewController](self, "viewController");
  v11 = -[UIViewController view](v10, "view");
  -[UIView addSubview:](v11, "addSubview:", self->_indicator);
  -[UIActivityIndicatorView startAnimating](self->_indicator, "startAnimating");
}


// ===== 0x003a3cc4  -[TapjoyManager showAdWallWithRootViewController:] =====
void __cdecl -[TapjoyManager showAdWallWithRootViewController:](TapjoyManager *self, SEL a2, id a3)
{
  +[Tapjoy showOffersWithViewController:](&OBJC_CLASS___Tapjoy, "showOffersWithViewController:", a3);
}


// ===== 0x003a5484  -[ADCPowerWallManager showAdWallWithRootViewController:] =====
void __cdecl -[ADCPowerWallManager showAdWallWithRootViewController:](ADCPowerWallManager *self, SEL a2, id a3)
{
  id v4; // r0

  v4 = +[AdWalls_OpenUDID value](&OBJC_CLASS___AdWalls_OpenUDID, "value");
  +[ADCPowerWallViewController showPowerWallViewFromViewController:siteId:siteKey:mediaId:userIdentifier:useReward:useSandBox:](
    &OBJC_CLASS___ADCPowerWallViewController,
    "showPowerWallViewFromViewController:siteId:siteKey:mediaId:userIdentifier:useReward:useSandBox:",
    a3,
    CFSTR("281"),
    CFSTR("673795f3ad5be0f8b72ff7194dc783c6"),
    CFSTR("126"),
    v4,
    1,
    0);
}


// ===== 0x00426b34  -[TMAdBanner rootViewController] =====
UIViewController *__cdecl -[TMAdBanner rootViewController](TMAdBanner *self, SEL a2)
{
  return self->_rootViewController;
}


// ===== 0x00426b44  -[TMAdBanner setRootViewController:] =====
void __cdecl -[TMAdBanner setRootViewController:](TMAdBanner *self, SEL a2, id a3)
{
  self->_rootViewController = (UIViewController *)a3;
}


// ===== 0x00482308  +[PunchBoxAd openInAppWhenNonJailBroken:] =====
void __cdecl +[PunchBoxAd openInAppWhenNonJailBroken:](id a1, SEL a2, char a3)
{
  ;
}


// ===== 0x0048508c  -[UIDeviceExtend isJailBroken] =====
char __cdecl -[UIDeviceExtend isJailBroken](UIDeviceExtend *self, SEL a2)
{
  return 0;
}


// ===== 0x004f217c  -[TMST_SessionInfo jailbroken] =====
int __cdecl -[TMST_SessionInfo jailbroken](TMST_SessionInfo *self, SEL a2)
{
  int result; // r0

  result = self->jailbroken;
  __dmb(0xBu);
  return result;
}


// ===== 0x004f2190  -[TMST_SessionInfo setJailbroken:] =====
void __cdecl -[TMST_SessionInfo setJailbroken:](TMST_SessionInfo *self, SEL a2, int a3)
{
  __dmb(0xBu);
  self->jailbroken = a3;
  __dmb(0xBu);
}


// ===== 0x004f6cf8  -[UIDevice isJailbroken] =====
char __cdecl -[UIDevice isJailbroken](UIDevice *self, SEL a2)
{
  return 0;
}


// ===== 0x004f8264  -[TaomeeAnalyticImplement innerApplicationDidBecomeActive] =====
void __cdecl -[TaomeeAnalyticImplement innerApplicationDidBecomeActive](TaomeeAnalyticImplement *self, SEL a2)
{
  TMST_DebugLogDelegate *v3; // r0
  TMST_SessionInfo *currentSession; // r5
  NSDate *v5; // r0
  double v6; // r0

  self->appState = 1;
  v3 = -[TaomeeAnalyticImplement dbgDlg](self, "dbgDlg");
  -[TMST_DebugLogDelegate writeLog:](v3, "writeLog:", CFSTR("App state changed to Foreground."));
  currentSession = self->currentSession;
  v5 = +[NSDate date](&OBJC_CLASS___NSDate, "date");
  LODWORD(v6) = -[NSDate timeIntervalSince1970](v5, "timeIntervalSince1970");
  -[TMST_SessionInfo setLastResumeStamp:](currentSession, "setLastResumeStamp:", (int)v6);
  -[TaomeeAnalyticImplement innerLogEvent:withEventType:isRealTime:withParameters:](
    self,
    "innerLogEvent:withEventType:isRealTime:withParameters:",
    CFSTR("SessionStart"),
    0,
    1,
    0);
  self->startTime_ = (int)CFAbsoluteTimeGetCurrent();
}


// ===== 0x004f8354  -[TaomeeAnalyticImplement applicationDidBecomeActive] =====
void __cdecl -[TaomeeAnalyticImplement applicationDidBecomeActive](TaomeeAnalyticImplement *self, SEL a2)
{
  -[TaomeeAnalyticImplement performSelector:onThread:withObject:waitUntilDone:](
    self,
    "performSelector:onThread:withObject:waitUntilDone:",
    "innerApplicationDidBecomeActive",
    self->thd,
    0,
    0);
}


// ===== 0x00561e94  +[TMIAPVerify deviceIsJailBroken] =====
char __cdecl +[TMIAPVerify deviceIsJailBroken](id a1, SEL a2)
{
  return +[TMDeviceChecker isJailBroken](&OBJC_CLASS___TMDeviceChecker, "isJailBroken");
}


// ===== 0x00561eb8  +[TMIAPVerify verifyPurchase:withRootViewController:] =====
char __cdecl +[TMIAPVerify verifyPurchase:withRootViewController:](id a1, SEL a2, id a3, id a4)
{
  TMLocalVerification *v6; // r0

  v6 = +[TMLocalVerification sharedInstance](&OBJC_CLASS___TMLocalVerification, "sharedInstance");
  return -[TMLocalVerification verifyPurchase:withRootViewController:](
           v6,
           "verifyPurchase:withRootViewController:",
           a3,
           a4);
}


// ===== 0x00562bd8  +[TMDeviceChecker isJailBroken] =====
char __cdecl +[TMDeviceChecker isJailBroken](id a1, SEL a2)
{
  return 0;
}


// ===== 0x005637d4  -[TMLocalVerification verifyPurchase:withRootViewController:] =====
char __cdecl -[TMLocalVerification verifyPurchase:withRootViewController:](
        TMLocalVerification *self,
        SEL a2,
        id a3,
        id a4)
{
  NSBundle *v6; // r0
  NSBundle *v7; // r0
  NSString *v8; // r8
  NSBundle *v9; // r0
  NSString *v10; // r0
  char v11; // r5
  UIAlertView *v12; // r4
  NSBundle *v13; // r0
  NSBundle *v14; // r0
  NSString *v15; // r4
  NSBundle *v16; // r0
  NSString *v17; // r0
  NSString *v19; // [sp+10h] [bp-24h]
  NSString *v20; // [sp+10h] [bp-24h]
  UIAlertView *v21; // [sp+14h] [bp-20h]
  UIAlertView *v22; // [sp+14h] [bp-20h]
  TMLocalVerification *v23; // [sp+18h] [bp-1Ch]
  TMLocalVerification *v24; // [sp+18h] [bp-1Ch]

  dword_B41450 = (int)a4;
  if ( +[TMDeviceChecker isInstalledCaker](&OBJC_CLASS___TMDeviceChecker, "isInstalledCaker") )
  {
    v21 = +[UIAlertView alloc](&OBJC_CLASS___UIAlertView, "alloc");
    v23 = self;
    v6 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
    v19 = -[NSBundle localizedStringForKey:value:table:](
            v6,
            "localizedStringForKey:value:table:",
            CFSTR("IAPCracker"),
            &stru_B0DA58,
            CFSTR("IAPVerifyStrings"));
    v7 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
    v8 = -[NSBundle localizedStringForKey:value:table:](
           v7,
           "localizedStringForKey:value:table:",
           CFSTR("btnOk"),
           &stru_B0DA58,
           CFSTR("IAPVerifyStrings"));
    v9 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
    v10 = -[NSBundle localizedStringForKey:value:table:](
            v9,
            "localizedStringForKey:value:table:",
            CFSTR("btnCancel"),
            &stru_B0DA58,
            CFSTR("IAPVerifyStrings"));
    v11 = 0;
    v12 = -[UIAlertView initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:](
            v21,
            "initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:",
            0,
            v19,
            v23,
            v8,
            v10,
            0);
    -[UIAlertView setTag:](v12, "setTag:", 10001);
LABEL_5:
    -[UIAlertView show](v12, "show");
    -[UIAlertView release](v12, "release");
    return v11;
  }
  v24 = self;
  v11 = +[TMLocalVerification isTransactionAndItsReceiptValid:](
          &OBJC_CLASS___TMLocalVerification,
          "isTransactionAndItsReceiptValid:",
          a3);
  if ( !v11 )
  {
    v22 = +[UIAlertView alloc](&OBJC_CLASS___UIAlertView, "alloc");
    v13 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
    v20 = -[NSBundle localizedStringForKey:value:table:](
            v13,
            "localizedStringForKey:value:table:",
            CFSTR("PaymentError"),
            &stru_B0DA58,
            CFSTR("IAPVerifyStrings"));
    v14 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
    v15 = -[NSBundle localizedStringForKey:value:table:](
            v14,
            "localizedStringForKey:value:table:",
            CFSTR("btnOk"),
            &stru_B0DA58,
            CFSTR("IAPVerifyStrings"));
    v16 = +[NSBundle mainBundle](&OBJC_CLASS___NSBundle, "mainBundle");
    v17 = -[NSBundle localizedStringForKey:value:table:](
            v16,
            "localizedStringForKey:value:table:",
            CFSTR("btnCancel"),
            &stru_B0DA58,
            CFSTR("IAPVerifyStrings"));
    v11 = 0;
    v12 = -[UIAlertView initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:](
            v22,
            "initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:",
            0,
            v20,
            v24,
            v15,
            v17,
            0);
    -[UIAlertView setTag:](v12, "setTag:", 10000);
    goto LABEL_5;
  }
  return v11;
}


// ===== 0x00575768  -[DianRuAdWall isJailbroken] =====
char __cdecl -[DianRuAdWall isJailbroken](DianRuAdWall *self, SEL a2)
{
  NSFileManager *v2; // r0
  unsigned __int8 v3; // r6
  NSFileManager *v4; // r0
  unsigned __int8 v5; // r0
  char v6; // r1
  int v7; // r0

  v2 = +[NSFileManager defaultManager](&OBJC_CLASS___NSFileManager, "defaultManager");
  v3 = -[NSFileManager fileExistsAtPath:](v2, "fileExistsAtPath:", CFSTR("/Applications/Cydia.app"));
  v4 = +[NSFileManager defaultManager](&OBJC_CLASS___NSFileManager, "defaultManager");
  v5 = -[NSFileManager fileExistsAtPath:](v4, "fileExistsAtPath:", CFSTR("/private/var/lib/apt/"));
  v6 = v3;
  if ( v3 )
    v6 = 1;
  v7 = (unsigned __int8)(v5 | v6);
  if ( v7 )
    LOBYTE(v7) = 0;
  return v7;
}


// ===== 0x0059e6b4  -[DMOfferWallViewController rootViewController] =====
UIViewController *__cdecl -[DMOfferWallViewController rootViewController](DMOfferWallViewController *self, SEL a2)
{
  return self->_rootViewController;
}


// ===== 0x0059e6c4  -[DMOfferWallViewController setRootViewController:] =====
void __cdecl -[DMOfferWallViewController setRootViewController:](DMOfferWallViewController *self, SEL a2, id a3)
{
  self->_rootViewController = (UIViewController *)a3;
}


// ===== 0x005c9200  -[immobView initWithAdUnitId:adUnitType:rootViewController:userInfo:] =====
immobView *__cdecl -[immobView initWithAdUnitId:adUnitType:rootViewController:userInfo:](
        immobView *self,
        SEL a2,
        id a3,
        int a4,
        id a5,
        id a6)
{
  immobView *v9; // r4
  immobView *v11; // r0
  LMCtrMgr *v12; // r0
  LMCtrMgr *v13; // r0
  LMSDKController *v14; // r0
  LMPlayerController *v15; // r0
  LMNetworkController *v16; // r0
  immobJavaScriptBridge *v17; // r0
  immobJavaScriptBridge *v18; // r0
  immobCache *v19; // r0
  immobCache *v20; // r0
  NSMutableArray *v21; // r0
  UIApplication *v22; // r0
  UIWindow *v23; // r0
  NSMutableDictionary *v24; // r0
  NSMutableDictionary *v25; // r0
  id v26; // r2
  id v27; // r2
  id v28; // r0
  Class v29; // r5
  SEL v30; // r6
  void (__fastcall *v31)(Class, SEL, int); // r0
  SEL v32; // r6
  void (__fastcall *v33)(Class, SEL); // r0
  objc_super v34; // [sp+Ch] [bp-20h] BYREF

  if ( !+[immobUtils isNotNULL:](&OBJC_CLASS___immobUtils, "isNotNULL:", a3)
    || (unsigned __int8)objc_msgSend(a3, "isEqualToString:", &stru_B0DA58) )
  {
    NSLog(&stru_B2B5A8.isa);
    return nullptr;
  }
  v34.receiver = self;
  v34.super_class = (Class)&OBJC_CLASS___immobView;
  v11 = -[immobView init](&v34, "init");
  v9 = v11;
  if ( !v11 )
    return nullptr;
  -[immobView initWithFrame:](
    v11,
    "initWithFrame:",
    CGRectZero.origin.x,
    CGRectZero.origin.y,
    CGRectZero.size.width,
    CGRectZero.size.height);
  v12 = +[LMCtrMgr alloc](&OBJC_CLASS___LMCtrMgr, "alloc");
  v13 = -[LMCtrMgr initWithImmobView:](v12, "initWithImmobView:", v9);
  v9->mLMCtrMgr = v13;
  v14 = -[LMCtrMgr mLMSDKController](v13, "mLMSDKController");
  -[LMSDKController setDelegate:](v14, "setDelegate:", v9);
  v15 = -[LMCtrMgr mLMPlayercontroller](v9->mLMCtrMgr, "mLMPlayercontroller");
  -[LMPlayerController setPlayerDelegate:](v15, "setPlayerDelegate:", v9);
  v16 = -[LMCtrMgr mLMNetworkController](v9->mLMCtrMgr, "mLMNetworkController");
  -[LMNetworkController setDelegate:](v16, "setDelegate:", v9);
  v17 = +[immobJavaScriptBridge alloc](&OBJC_CLASS___immobJavaScriptBridge, "alloc");
  v18 = -[immobJavaScriptBridge init](v17, "init");
  v9->immobjavaScriptBridge = v18;
  -[immobJavaScriptBridge setBridgeDelegate:](v18, "setBridgeDelegate:", v9);
  v19 = +[immobCache alloc](&OBJC_CLASS___immobCache, "alloc");
  v20 = -[immobCache init](v19, "init");
  v9->immobFrameWork = v20;
  -[immobCache setDelegate:](v20, "setDelegate:", v9);
  v21 = +[NSMutableArray alloc](&OBJC_CLASS___NSMutableArray, "alloc");
  v9->scoreKeyArray = -[NSMutableArray initWithCapacity:](v21, "initWithCapacity:", 5);
  -[immobView setAdUnitIdString:](v9, "setAdUnitIdString:", a3);
  if ( !a5 )
  {
    v22 = +[UIApplication sharedApplication](&OBJC_CLASS___UIApplication, "sharedApplication");
    v23 = -[UIApplication keyWindow](v22, "keyWindow");
    -[UIWindow rootViewController](v23, "rootViewController");
  }
  -[immobView setRootViewController:](v9, "setRootViewController:");
  v24 = +[NSMutableDictionary alloc](&OBJC_CLASS___NSMutableDictionary, "alloc");
  if ( a6 )
    v25 = -[NSMutableDictionary initWithDictionary:](v24, "initWithDictionary:", a6);
  else
    v25 = -[NSMutableDictionary init](v24, "init");
  v9->UserAttribute = v25;
  -[immobView setAdUnitIdType:](v9, "setAdUnitIdType:", a4);
  if ( -[NSMutableDictionary objectForKey:](v9->UserAttribute, "objectForKey:", CFSTR("accountname")) )
  {
    v26 = -[NSMutableDictionary objectForKey:](v9->UserAttribute, "objectForKey:", CFSTR("accountname"));
    -[immobView setGameIdString:](v9, "setGameIdString:", v26);
  }
  else
  {
    -[immobView setGameIdString:](v9, "setGameIdString:", CFSTR("null"));
  }
  if ( -[NSMutableDictionary objectForKey:](v9->UserAttribute, "objectForKey:", CFSTR("channelID")) )
  {
    v27 = -[NSMutableDictionary objectForKey:](v9->UserAttribute, "objectForKey:", CFSTR("channelID"));
    -[immobView setChannelIdString:](v9, "setChannelIdString:", v27);
  }
  else
  {
    -[immobView setChannelIdString:](v9, "setChannelIdString:", CFSTR("null"));
  }
  -[immobCache setAdUnitIdString:](v9->immobFrameWork, "setAdUnitIdString:", a3);
  -[immobView setIsOff:](v9, "setIsOff:", 0);
  v28 = (id)-[immobCache checkImmobSDKFWState](v9->immobFrameWork, "checkImmobSDKFWState");
  -[immobView setIsAdReady:](v9, "setIsAdReady:", v28);
  v9->isPageFinshLoad = 0;
  v9->isFWPVEqualSDKPV = -[immobCache checkFWPV](v9->immobFrameWork, "checkFWPV");
  v29 = NSClassFromString(&cfstr_Talkingdatasdk.isa);
  if ( v29 )
  {
    v9->isSupportTalkingData = 1;
    v30 = NSSelectorFromString(&cfstr_Setsilentmode.isa);
    v31 = (void (__fastcall *)(Class, SEL, int))-[objc_class methodForSelector:](v29, "methodForSelector:", v30);
    v31(v29, v30, 1);
    v32 = NSSelectorFromString(&cfstr_Init.isa);
    v33 = (void (__fastcall *)(Class, SEL))-[objc_class methodForSelector:](v29, "methodForSelector:", v32);
    v33(v29, v32);
  }
  else
  {
    v9->isSupportTalkingData = 0;
  }
  return v9;
}


// ===== 0x005d2678  -[immobView rootViewController] =====
UIViewController *__cdecl -[immobView rootViewController](immobView *self, SEL a2)
{
  return self->rootViewController;
}


