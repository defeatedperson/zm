import { createApp } from "vue";
import ElementPlus from "element-plus";
import zhCn from "element-plus/es/locale/lang/zh-cn";
import "element-plus/dist/index.css";
import "element-plus/theme-chalk/dark/css-vars.css";
import App from "./App.vue";

// 桌面端整包引入即可（无网络加载顾虑）；dark 变量由 index.html 的
// <html class="dark"> 激活（与参考项目同一套接法）
createApp(App).use(ElementPlus, { locale: zhCn }).mount("#app");
