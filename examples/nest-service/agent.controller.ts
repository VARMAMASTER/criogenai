import { Body, Controller, Get, HttpException, Post } from "@nestjs/common";
import { AgentService } from "./agent.service";

@Controller("agent")
export class AgentController {
  constructor(private readonly agents: AgentService) {}

  @Post()
  async ask(@Body() body: { question: string }) {
    const result = await this.agents.ask(body.question);
    return { answer: result.text, model: result.answeredBy, durationMs: result.durationMs, usage: result.usage };
  }

  @Get("health")
  async health() {
    const report = await this.agents.health();
    if (!report.ok) throw new HttpException(report, 503);
    return report;
  }
}
